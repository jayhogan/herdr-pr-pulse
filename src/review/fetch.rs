//! The Review view's background fetch: like `fetch.rs`, one worker thread per copy sharing
//! `review-cache.json` through its own lock, so it never waits on the other views' fetch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::fetch::{FetchRequest, Plan, RepoStatus, decide};
use crate::github::{Gh, GhError};
use crate::review::github;
use crate::review::model::ReviewPr;
use crate::store::StateDir;

pub const CACHE: &str = "review-cache.json";
pub const LOCK: &str = "review-fetch.lock";
const CACHE_VERSION: u32 = 1;
/// Most PRs per detail request, as in `fetch.rs`.
const DETAIL_CHUNK: usize = 25;

/// `review-cache.json`: the last review fetch, shared by all copies. Every fetched PR is
/// kept; the app decides which to list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReviewCache {
    pub version: u32,
    pub fetched_at: Option<DateTime<Utc>>,
    pub viewer: String,
    /// The configured repos this was fetched for.
    #[serde(default)]
    pub repos: Vec<String>,
    /// org → the viewer's team slugs; None when the lookup failed (no ★, notice shown).
    pub teams: Option<BTreeMap<String, Vec<String>>>,
    pub prs: Vec<ReviewPr>,
    pub error: Option<GhError>,
    pub error_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReviewEvent {
    Started,
    Progress(Vec<(String, RepoStatus)>),
    Done(ReviewCache),
}

/// Starts the worker for `repos`. It exits when the request sender is dropped or the
/// event receiver is gone.
pub fn spawn(
    gh: Box<dyn Gh>,
    state: StateDir,
    repos: Vec<String>,
) -> (Sender<FetchRequest>, Receiver<ReviewEvent>) {
    let (req_tx, req_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::spawn(move || {
        let worker = Worker {
            gh,
            state,
            repos,
            events: ev_tx,
        };
        for req in req_rx {
            if worker.handle(&req).is_err() {
                return;
            }
        }
    });
    (req_tx, ev_rx)
}

/// The same repos, in any order and letter case.
fn same_repos(a: &[String], b: &[String]) -> bool {
    let set = |r: &[String]| -> BTreeSet<String> { r.iter().map(|r| r.to_lowercase()).collect() };
    set(a) == set(b)
}

/// The UI stopped listening.
struct Gone;

struct Worker {
    gh: Box<dyn Gh>,
    state: StateDir,
    repos: Vec<String>,
    events: Sender<ReviewEvent>,
}

impl Worker {
    fn send(&self, ev: ReviewEvent) -> Result<(), Gone> {
        self.events.send(ev).map_err(|_| Gone)
    }

    /// The cache on disk; an unreadable one counts as empty.
    fn load(&self) -> ReviewCache {
        self.state.load(CACHE).unwrap_or_default()
    }

    /// As `fetch::Worker::handle`, with this view's lock and cache. A `Recheck` is never
    /// sent here; it would fetch like `Forced`.
    fn handle(&self, req: &FetchRequest) -> Result<(), Gone> {
        let (mut lock, have_lock) = match self.state.try_lock_named(LOCK) {
            Ok(Some(file)) => (Some(file), true),
            Ok(None) => (None, false),
            Err(_) => (None, true),
        };
        let cache = self.load();
        // A copy launched with other repos wrote it: as good as no cache.
        let age = cache
            .fetched_at
            .max(cache.error_at)
            .filter(|_| same_repos(&cache.repos, &self.repos))
            .map(|t| Utc::now() - t);
        match decide(req, age, have_lock) {
            Plan::UseCache => self.send(ReviewEvent::Done(cache)),
            Plan::WaitForOther => {
                drop(self.state.lock_named(LOCK));
                self.send(ReviewEvent::Done(self.load()))
            }
            Plan::Fetch => {
                self.send(ReviewEvent::Started)?;
                if lock.is_none() {
                    lock = self.state.lock_named(LOCK).ok();
                }
                let result = self.fetch_all()?;
                let cache = self.save(result);
                drop(lock);
                self.send(ReviewEvent::Done(cache))
            }
        }
    }

    /// Search, then detail per repo (reporting progress), then the viewer's teams per org.
    /// The outer error means the UI is gone.
    fn fetch_all(&self) -> Result<Result<ReviewCache, GhError>, Gone> {
        let started = Utc::now();
        let (viewer, hits) = match github::search(self.gh.as_ref(), &self.repos) {
            Ok(found) => found,
            Err(e) => return Ok(Err(e)),
        };
        // Configured repos first, in config order; GitHub may spell a repo with other case.
        let mut by_repo: Vec<(String, Vec<String>)> =
            self.repos.iter().map(|r| (r.clone(), Vec::new())).collect();
        for (id, repo) in hits {
            match by_repo
                .iter_mut()
                .find(|(r, _)| r.eq_ignore_ascii_case(&repo))
            {
                Some((_, ids)) => ids.push(id),
                None => by_repo.push((repo, vec![id])),
            }
        }
        let mut status: Vec<(String, RepoStatus)> = by_repo
            .iter()
            .map(|(r, ids)| {
                let s = if ids.is_empty() {
                    RepoStatus::Done(0)
                } else {
                    RepoStatus::Queued
                };
                (r.clone(), s)
            })
            .collect();
        self.send(ReviewEvent::Progress(status.clone()))?;
        let mut prs: Vec<ReviewPr> = Vec::new();
        for (i, (_, ids)) in by_repo.iter().enumerate() {
            if ids.is_empty() {
                continue;
            }
            status[i].1 = RepoStatus::Loading;
            self.send(ReviewEvent::Progress(status.clone()))?;
            let mut got = Vec::new();
            for chunk in ids.chunks(DETAIL_CHUNK) {
                match github::detail(self.gh.as_ref(), chunk) {
                    Ok(more) => got.extend(more),
                    Err(e) => return Ok(Err(e)),
                }
            }
            status[i].1 = RepoStatus::Done(got.len());
            self.send(ReviewEvent::Progress(status.clone()))?;
            for pr in got {
                if !prs.iter().any(|p| p.pr.id == pr.pr.id) {
                    prs.push(pr);
                }
            }
        }
        let orgs: BTreeSet<String> = prs.iter().map(|p| p.org().to_string()).collect();
        let teams = orgs
            .into_iter()
            .map(|org| {
                let slugs = github::teams(self.gh.as_ref(), &org, &viewer)?;
                Ok((org, slugs))
            })
            .collect::<Result<BTreeMap<_, _>, GhError>>()
            .ok();
        Ok(Ok(ReviewCache {
            version: CACHE_VERSION,
            fetched_at: Some(started),
            viewer,
            repos: self.repos.clone(),
            teams,
            prs,
            error: None,
            error_at: None,
        }))
    }

    /// Writes the outcome into `review-cache.json` under its lock and returns the cache as
    /// written. A failed fetch keeps the last PRs and records the error.
    fn save(&self, result: Result<ReviewCache, GhError>) -> ReviewCache {
        let now = Utc::now();
        let apply = |c: &mut ReviewCache| match &result {
            Ok(new) => *c = new.clone(),
            Err(e) => {
                c.error = Some(e.clone());
                c.error_at = Some(now);
            }
        };
        self.state
            .update(CACHE, |c: &mut ReviewCache| {
                apply(c);
                true
            })
            .unwrap_or_else(|io| {
                let mut c = self.load();
                apply(&mut c);
                c.error = Some(GhError::Other(format!("could not write {CACHE}: {io}")));
                c.error_at = Some(now);
                c
            })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use chrono::Duration;

    use super::*;
    use crate::github::fake::FakeGh;

    const SEARCH: &str = include_str!("../../tests/fixtures/review-search.json");
    const DETAIL: &str = include_str!("../../tests/fixtures/review-detail.json");
    const TEAMS: &str = include_str!("../../tests/fixtures/review-teams.json");

    fn repos() -> Vec<String> {
        [
            "heard/heard-ts",
            "heard/tax-engine",
            "heard/docket",
            "heard/empty",
        ]
        .map(String::from)
        .to_vec()
    }

    fn fake() -> FakeGh {
        FakeGh::new()
            .with("review-search", Ok(SEARCH.into()))
            .with("review-detail", Ok(DETAIL.into()))
            .with("review-teams", Ok(TEAMS.into()))
    }

    fn until_done(rx: &Receiver<ReviewEvent>) -> Vec<ReviewEvent> {
        let mut out = Vec::new();
        loop {
            let ev = rx
                .recv_timeout(StdDuration::from_secs(10))
                .expect("worker event");
            let done = matches!(ev, ReviewEvent::Done(_));
            out.push(ev);
            if done {
                return out;
            }
        }
    }

    fn done(evs: &[ReviewEvent]) -> ReviewCache {
        let Some(ReviewEvent::Done(c)) = evs.last() else {
            panic!("no Done")
        };
        c.clone()
    }

    #[test]
    fn full_fetch_writes_cache_with_teams() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = spawn(Box::new(fake()), StateDir::new(tmp.path().into()), repos());
        tx.send(FetchRequest::Forced).unwrap();
        let evs = until_done(&rx);
        assert_eq!(evs[0], ReviewEvent::Started);
        let c = done(&evs);
        assert_eq!(c.viewer, "jayhogan");
        assert_eq!(c.prs.len(), 14);
        assert_eq!(
            c.teams,
            Some(BTreeMap::from([(
                "heard".to_string(),
                vec!["billing".to_string(), "platform".to_string()]
            )]))
        );
        assert_eq!(c.error, None);
        assert!(c.fetched_at.is_some());
        assert!(tmp.path().join(CACHE).exists());
        // Every configured repo shows, in config order; one with no hits is done at once.
        let Some(ReviewEvent::Progress(first)) = evs.get(1) else {
            panic!("no progress")
        };
        let names: Vec<&str> = first.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(names, repos());
        assert_eq!(first[3].1, RepoStatus::Done(0));
        let ReviewEvent::Progress(last) = &evs[evs.len() - 2] else {
            panic!("no final progress")
        };
        assert!(last.iter().all(|(_, s)| matches!(s, RepoStatus::Done(_))));
    }

    #[test]
    fn team_failure_keeps_list_without_teams() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = fake().with(
            "review-teams",
            Err(GhError::Other("INSUFFICIENT_SCOPES".into())),
        );
        let (tx, rx) = spawn(Box::new(gh), StateDir::new(tmp.path().into()), repos());
        tx.send(FetchRequest::Forced).unwrap();
        let c = done(&until_done(&rx));
        assert_eq!(c.teams, None);
        assert_eq!(c.prs.len(), 14);
        assert_eq!(c.error, None);
    }

    fn seeded(tmp: &std::path::Path, fetched_ago: i64) -> (StateDir, ReviewCache) {
        let state = StateDir::new(tmp.into());
        let prs = crate::review::model::parse_review_detail(DETAIL).unwrap();
        let cache = ReviewCache {
            version: CACHE_VERSION,
            fetched_at: Some(Utc::now() - Duration::seconds(fetched_ago)),
            viewer: "jayhogan".into(),
            repos: repos(),
            teams: Some(BTreeMap::new()),
            prs: prs.into_iter().take(1).collect(),
            error: None,
            error_at: None,
        };
        state
            .update(CACHE, |c: &mut ReviewCache| {
                *c = cache.clone();
                true
            })
            .unwrap();
        (state, cache)
    }

    #[test]
    fn search_failure_keeps_previous_prs() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _) = seeded(tmp.path(), 300);
        let gh = fake().with("review-search", Err(GhError::LoginExpired));
        let (tx, rx) = spawn(Box::new(gh), state, repos());
        tx.send(FetchRequest::Forced).unwrap();
        let c = done(&until_done(&rx));
        assert_eq!(c.prs.len(), 1);
        assert_eq!(c.error, Some(GhError::LoginExpired));
        assert!(c.error_at.is_some());
    }

    #[test]
    fn scheduled_uses_fresh_cache_without_gh() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, cache) = seeded(tmp.path(), 10);
        // No canned responses: any gh call would fail the fetch.
        let (tx, rx) = spawn(Box::new(FakeGh::new()), state, repos());
        tx.send(FetchRequest::Scheduled).unwrap();
        assert_eq!(until_done(&rx), vec![ReviewEvent::Done(cache)]);
    }

    #[test]
    fn cache_for_other_repos_is_refetched() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _) = seeded(tmp.path(), 10);
        let other = vec!["heard/other".to_string()];
        let (tx, rx) = spawn(Box::new(fake()), state, other.clone());
        tx.send(FetchRequest::Scheduled).unwrap();
        let evs = until_done(&rx);
        assert_eq!(evs[0], ReviewEvent::Started);
        assert_eq!(done(&evs).repos, other);
    }

    #[test]
    fn same_repos_in_other_case_use_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, cache) = seeded(tmp.path(), 10);
        let upper = repos().iter().map(|r| r.to_uppercase()).collect();
        let (tx, rx) = spawn(Box::new(FakeGh::new()), state, upper);
        tx.send(FetchRequest::Scheduled).unwrap();
        assert_eq!(until_done(&rx), vec![ReviewEvent::Done(cache)]);
    }

    #[test]
    fn review_lock_is_separate_from_base_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().into());
        let base = state.try_fetch_lock().unwrap();
        assert!(base.is_some());
        let review = state.try_lock_named(LOCK).unwrap();
        assert!(review.is_some());
        assert!(state.try_lock_named(LOCK).unwrap().is_none());
        drop(review);
        assert!(crate::store::tests::relocks(&state, LOCK));
    }
}
