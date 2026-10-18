//! Background GitHub fetch (spec §5): one worker thread per instance, sharing one
//! `cache.json` between all running copies through the fetch lock.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, Utc};

use crate::github::{self, Gh, GhError, Target};
use crate::model::Pr;
use crate::store::{Cache, StateDir};

const CACHE: &str = "cache.json";
const CACHE_VERSION: u32 = 1;
/// A cache younger than this is used as is by a scheduled request.
const FRESH: i64 = 60;
/// Most PRs per detail request (cost for larger batches is untested).
const DETAIL_CHUNK: usize = 25;
const POLL_EVERY: StdDuration = StdDuration::from_secs(60);
const RECHECK_AFTER: StdDuration = StdDuration::from_secs(10);
const WAIT_FOR_OTHER: StdDuration = StdDuration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum FetchRequest {
    Scheduled,
    Forced,
    Recheck(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepoStatus {
    Queued,
    Loading,
    Done(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FetchEvent {
    Started,
    Progress(Vec<(String, RepoStatus)>),
    Done(Cache),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    UseCache,
    Fetch,
    WaitForOther,
}

pub fn decide(req: &FetchRequest, cache_age: Option<Duration>, have_lock: bool) -> Plan {
    match req {
        FetchRequest::Forced | FetchRequest::Recheck(_) => Plan::Fetch,
        FetchRequest::Scheduled => match cache_age {
            Some(age) if age < Duration::seconds(FRESH) => Plan::UseCache,
            _ if have_lock => Plan::Fetch,
            _ => Plan::WaitForOther,
        },
    }
}

/// `detail` in requests of at most `DETAIL_CHUNK` targets.
fn detail_chunked(gh: &dyn Gh, targets: &[Target]) -> Result<Vec<Pr>, GhError> {
    let mut out = Vec::new();
    for chunk in targets.chunks(DETAIL_CHUNK) {
        out.extend(github::detail(gh, chunk)?);
    }
    Ok(out)
}

/// Adds `new` to `prs`, replacing any PR with the same id.
fn merge_prs(prs: &mut Vec<Pr>, new: Vec<Pr>) {
    for pr in new {
        match prs.iter_mut().find(|p| p.id == pr.id) {
            Some(slot) => *slot = pr,
            None => prs.push(pr),
        }
    }
}

/// Decides when the main loop sends requests: `Scheduled` at startup and every 60 s, and
/// one `Recheck` 10 s after a fetch that left merge states unknown.
pub struct Poller {
    next_scheduled: Instant,
    recheck: Option<(Instant, Vec<String>)>,
    /// Requests sent and not yet answered by a `Done`; true for a `Recheck`.
    in_flight: VecDeque<bool>,
}

impl Poller {
    pub fn new(now: Instant) -> Poller {
        Poller {
            next_scheduled: now,
            recheck: None,
            in_flight: VecDeque::new(),
        }
    }

    /// Requests due at `now`; each returned request must be sent.
    pub fn due(&mut self, now: Instant) -> Vec<FetchRequest> {
        let mut out = Vec::new();
        if now >= self.next_scheduled {
            self.next_scheduled = now + POLL_EVERY;
            self.in_flight.push_back(false);
            out.push(FetchRequest::Scheduled);
        }
        if self.recheck.as_ref().is_some_and(|(at, _)| now >= *at) {
            let (_, ids) = self.recheck.take().expect("checked above");
            self.in_flight.push_back(true);
            out.push(FetchRequest::Recheck(ids));
        }
        out
    }

    /// The request for `r`.
    pub fn forced(&mut self) -> FetchRequest {
        self.in_flight.push_back(false);
        FetchRequest::Forced
    }

    /// Called for every `Done`, with the app's unknown merge ids after applying it.
    pub fn on_done(&mut self, now: Instant, unknown: Vec<String>) {
        if self.in_flight.pop_front().unwrap_or(false) {
            return; // a recheck's answer never schedules another
        }
        self.recheck = (!unknown.is_empty()).then(|| (now + RECHECK_AFTER, unknown));
    }
}

/// Starts the worker. It exits when the request sender is dropped or the event
/// receiver is gone.
pub fn spawn(gh: Box<dyn Gh>, state: StateDir) -> (Sender<FetchRequest>, Receiver<FetchEvent>) {
    let (req_tx, req_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::spawn(move || {
        let worker = Worker {
            gh,
            state,
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

/// The UI stopped listening.
struct Gone;

struct Worker {
    gh: Box<dyn Gh>,
    state: StateDir,
    events: Sender<FetchEvent>,
}

impl Worker {
    fn send(&self, ev: FetchEvent) -> Result<(), Gone> {
        self.events.send(ev).map_err(|_| Gone)
    }

    /// The cache on disk; an unreadable one counts as empty.
    fn load(&self) -> Cache {
        self.state.load(CACHE).unwrap_or_default()
    }

    fn handle(&self, req: &FetchRequest) -> Result<(), Gone> {
        // The lock is held until this function returns; a manual request waits for it.
        let mut lock = self.state.try_fetch_lock().ok().flatten();
        let cache = self.load();
        let age = cache.fetched_at.map(|t| Utc::now() - t);
        match decide(req, age, lock.is_some()) {
            Plan::UseCache => self.send(FetchEvent::Done(cache)),
            Plan::WaitForOther => {
                thread::sleep(WAIT_FOR_OTHER);
                self.send(FetchEvent::Done(self.load()))
            }
            Plan::Fetch => {
                self.send(FetchEvent::Started)?;
                if lock.is_none() {
                    lock = self.state.fetch_lock().ok();
                }
                let result = match req {
                    FetchRequest::Recheck(ids) => self.recheck(ids),
                    _ => self.fetch_all()?,
                };
                let cache = self.save(result);
                drop(lock);
                self.send(FetchEvent::Done(cache))
            }
        }
    }

    /// Steps 1-3 of spec §5. The outer error means the UI is gone (nothing is saved).
    fn fetch_all(&self) -> Result<Result<Change, GhError>, Gone> {
        let started = Utc::now();
        let search = match github::search(self.gh.as_ref()) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let mut seen = HashSet::new();
        let mut by_repo: BTreeMap<String, Vec<Target>> = BTreeMap::new();
        for hit in search.authored.iter().chain(&search.assigned) {
            if seen.insert(hit.id.clone()) {
                by_repo.entry(hit.repo.clone()).or_default().push(Target {
                    id: hit.id.clone(),
                    head_ref: Some(hit.head_ref.clone()),
                });
            }
        }
        let mut status: Vec<(String, RepoStatus)> = by_repo
            .keys()
            .map(|r| (r.clone(), RepoStatus::Queued))
            .collect();
        let mut prs = Vec::new();
        for (i, targets) in by_repo.values().enumerate() {
            status[i].1 = RepoStatus::Loading;
            self.send(FetchEvent::Progress(status.clone()))?;
            let got = match detail_chunked(self.gh.as_ref(), targets) {
                Ok(got) => got,
                Err(e) => return Ok(Err(e)),
            };
            status[i].1 = RepoStatus::Done(got.len());
            self.send(FetchEvent::Progress(status.clone()))?;
            merge_prs(&mut prs, got);
        }
        let fill: Vec<Target> = github::missing_stack_entries(&prs)
            .into_iter()
            .map(|id| Target { id, head_ref: None })
            .collect();
        match detail_chunked(self.gh.as_ref(), &fill) {
            Ok(got) => merge_prs(&mut prs, got),
            Err(e) => return Ok(Err(e)),
        }
        let ids = |hits: &[crate::model::SearchHit]| hits.iter().map(|h| h.id.clone()).collect();
        Ok(Ok(Change::Full(Cache {
            version: CACHE_VERSION,
            fetched_at: Some(started),
            viewer: search.viewer,
            authored: ids(&search.authored),
            assigned: ids(&search.assigned),
            prs,
            error: None,
            error_at: None,
        })))
    }

    fn recheck(&self, ids: &[String]) -> Result<Change, GhError> {
        let cache = self.load();
        let targets: Vec<Target> = ids
            .iter()
            .map(|id| {
                // Keep the behind-by comparison for PRs that came from search.
                let searched = cache.authored.contains(id) || cache.assigned.contains(id);
                let head_ref = cache
                    .prs
                    .iter()
                    .find(|p| &p.id == id && searched)
                    .map(|p| p.head_ref.clone());
                Target {
                    id: id.clone(),
                    head_ref,
                }
            })
            .collect();
        detail_chunked(self.gh.as_ref(), &targets).map(Change::Merge)
    }

    /// Writes the outcome into `cache.json` (under its lock, against the latest copy) and
    /// returns the cache as written. A write failure is reported in the returned cache.
    fn save(&self, result: Result<Change, GhError>) -> Cache {
        let now = Utc::now();
        let apply = |c: &mut Cache| match &result {
            Ok(Change::Full(new)) => *c = new.clone(),
            Ok(Change::Merge(prs)) => {
                merge_prs(&mut c.prs, prs.clone());
                c.error = None;
                c.error_at = None;
            }
            Err(e) => {
                c.error = Some(e.clone());
                c.error_at = Some(now);
            }
        };
        self.state
            .update(CACHE, |c: &mut Cache| {
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

/// What a successful fetch does to the cache.
enum Change {
    /// A full fetch replaces it.
    Full(Cache),
    /// A recheck replaces just these PRs.
    Merge(Vec<Pr>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fake::FakeGh;

    const SEARCH: &str = include_str!("../tests/fixtures/search.json");
    const DETAIL: &str = include_str!("../tests/fixtures/detail.json");

    #[test]
    fn decide_table() {
        let s = |secs| Some(Duration::seconds(secs));
        use FetchRequest::*;
        assert_eq!(decide(&Forced, s(5), false), Plan::Fetch);
        assert_eq!(decide(&Recheck(vec!["A".into()]), s(5), false), Plan::Fetch);
        assert_eq!(decide(&Scheduled, s(30), true), Plan::UseCache);
        assert_eq!(decide(&Scheduled, s(30), false), Plan::UseCache);
        assert_eq!(decide(&Scheduled, s(90), true), Plan::Fetch);
        assert_eq!(decide(&Scheduled, None, true), Plan::Fetch);
        assert_eq!(decide(&Scheduled, s(90), false), Plan::WaitForOther);
        assert_eq!(decide(&Scheduled, None, false), Plan::WaitForOther);
    }

    fn recv(rx: &Receiver<FetchEvent>) -> FetchEvent {
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker event")
    }

    /// Events up to and including the next `Done`.
    fn until_done(rx: &Receiver<FetchEvent>) -> Vec<FetchEvent> {
        let mut out = Vec::new();
        loop {
            let ev = recv(rx);
            let done = matches!(ev, FetchEvent::Done(_));
            out.push(ev);
            if done {
                return out;
            }
        }
    }

    #[test]
    fn worker_uses_fake_gh() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = FakeGh::new()
            .with("search", Ok(SEARCH.into()))
            .with("detail", Ok(DETAIL.into()));
        let (tx, rx) = spawn(Box::new(gh), StateDir::new(tmp.path().to_path_buf()));
        tx.send(FetchRequest::Scheduled).unwrap();
        let evs = until_done(&rx);
        assert_eq!(evs[0], FetchEvent::Started);
        let progress: Vec<_> = evs[1..evs.len() - 1].to_vec();
        assert!(!progress.is_empty());
        assert!(
            progress
                .iter()
                .all(|e| matches!(e, FetchEvent::Progress(_)))
        );
        let FetchEvent::Progress(last) = progress.last().unwrap() else {
            unreachable!()
        };
        let repos: Vec<&str> = last.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(repos, ["heard/docket", "heard/heard-ts"]);
        assert!(last.iter().all(|(_, s)| matches!(s, RepoStatus::Done(_))));
        let FetchEvent::Done(cache) = evs.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(cache.prs.len(), 9);
        assert_eq!(cache.viewer, "jayhogan");
        assert_eq!(cache.authored.len(), 7);
        assert_eq!(cache.assigned, ["PR_415", "PR_1279"]);
        assert!(cache.fetched_at.is_some());
        assert_eq!(cache.error, None);
        assert!(tmp.path().join("cache.json").exists());

        // A second Scheduled request within 60 s is served from the cache.
        tx.send(FetchRequest::Scheduled).unwrap();
        assert_eq!(until_done(&rx), vec![FetchEvent::Done(cache.clone())]);
    }

    #[test]
    fn worker_keeps_prs_on_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let good: Cache = state
            .update("cache.json", |c: &mut Cache| {
                c.viewer = "jayhogan".into();
                c.prs = crate::model::parse_detail(DETAIL).unwrap();
                true
            })
            .unwrap();
        let gh = FakeGh::new().with("search", Err(crate::github::GhError::LoginExpired));
        let (tx, rx) = spawn(Box::new(gh), state);
        tx.send(FetchRequest::Forced).unwrap();
        let evs = until_done(&rx);
        let FetchEvent::Done(cache) = evs.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(cache.prs, good.prs);
        assert_eq!(cache.error, Some(crate::github::GhError::LoginExpired));
        assert!(cache.error_at.is_some());
        let on_disk: Cache = StateDir::new(tmp.path().to_path_buf())
            .load("cache.json")
            .unwrap();
        assert_eq!(&on_disk, cache);
    }

    #[test]
    fn recheck_merges_into_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let mut prs = crate::model::parse_detail(DETAIL).unwrap();
        prs.retain(|p| p.id != "PR_1302");
        prs[0].title = "old title".into();
        state
            .update("cache.json", |c: &mut Cache| {
                c.prs = prs.clone();
                true
            })
            .unwrap();
        let gh = FakeGh::new().with("detail", Ok(DETAIL.into()));
        let (tx, rx) = spawn(Box::new(gh), state);
        tx.send(FetchRequest::Recheck(vec![prs[0].id.clone()]))
            .unwrap();
        let evs = until_done(&rx);
        let FetchEvent::Done(cache) = evs.last().unwrap() else {
            unreachable!()
        };
        // Every PR the response carried replaces or joins the cached ones, by id.
        assert_eq!(cache.prs.len(), 9);
        assert_ne!(cache.prs[0].title, "old title");
    }

    #[test]
    fn detail_is_chunked_by_25() {
        let gh = FakeGh::new().with("detail", Ok(DETAIL.into()));
        let targets: Vec<Target> = (0..26)
            .map(|n| Target {
                id: format!("PR_{n}"),
                head_ref: None,
            })
            .collect();
        detail_chunked(&gh, &targets).unwrap();
        let calls = gh.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1.as_object().unwrap().len(), 25);
        assert_eq!(calls[1].1, serde_json::json!({"id0": "PR_25"}));
    }

    #[test]
    fn poller_schedules_and_rechecks_once() {
        use std::time::Duration as D;
        let t0 = Instant::now();
        let mut p = Poller::new(t0);
        assert_eq!(p.due(t0), vec![FetchRequest::Scheduled]);
        assert!(p.due(t0 + D::from_secs(59)).is_empty());
        p.on_done(t0 + D::from_secs(2), vec!["A".into()]);
        assert!(p.due(t0 + D::from_secs(11)).is_empty());
        assert_eq!(
            p.due(t0 + D::from_secs(12)),
            vec![FetchRequest::Recheck(vec!["A".into()])]
        );
        // The recheck's own Done does not schedule another, even if A is still unknown.
        p.on_done(t0 + D::from_secs(13), vec!["A".into()]);
        assert!(p.due(t0 + D::from_secs(40)).is_empty());
        assert_eq!(p.due(t0 + D::from_secs(60)), vec![FetchRequest::Scheduled]);
        assert!(p.due(t0 + D::from_secs(61)).is_empty());
        assert_eq!(p.forced(), FetchRequest::Forced);
        // Done for the Scheduled at 60 s: rechecks again (once per fetch).
        p.on_done(t0 + D::from_secs(62), vec!["A".into()]);
        // Done for the forced fetch: nothing unknown any more, so the recheck is dropped.
        p.on_done(t0 + D::from_secs(63), vec![]);
        assert!(p.due(t0 + D::from_secs(80)).is_empty());
        assert_eq!(p.due(t0 + D::from_secs(120)), vec![FetchRequest::Scheduled]);
    }

    #[test]
    fn worker_exits_when_receiver_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = FakeGh::new()
            .with("search", Ok(SEARCH.into()))
            .with("detail", Ok(DETAIL.into()));
        let (tx, rx) = spawn(Box::new(gh), StateDir::new(tmp.path().to_path_buf()));
        drop(rx);
        // The worker notices on its first send and stops; later sends then fail.
        let _ = tx.send(FetchRequest::Forced);
        let gone = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            tx.send(FetchRequest::Scheduled).is_err()
        });
        assert!(gone);
    }
}
