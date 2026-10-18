//! Background GitHub fetch (spec §5): one worker thread per instance, sharing one
//! `cache.json` between all running copies through the fetch lock.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};

use crate::changes::{CHANGES_FILE, Changes};
use crate::github::{self, Gh, GhError, Target};
use crate::model::Pr;
use crate::store::{Cache, StateDir};
use crate::workspaces::{self, Memory};

const CACHE: &str = "cache.json";
const CACHE_VERSION: u32 = 1;
/// A cache younger than this is used as is by a scheduled request.
const FRESH: i64 = 60;
/// Most PRs per detail request (cost for larger batches is untested).
const DETAIL_CHUNK: usize = 25;
const POLL_EVERY: StdDuration = StdDuration::from_secs(60);
const RECHECK_AFTER: StdDuration = StdDuration::from_secs(10);

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
    /// `None` while a scheduled answer is awaited: the next one is timed from its `Done`, so
    /// the cache it carries is at least 60 s old by then.
    next_scheduled: Option<Instant>,
    recheck: Option<(Instant, Vec<String>)>,
    /// The fetch the last recheck was scheduled for (once per fetch, across copies).
    rechecked: Option<Option<DateTime<Utc>>>,
    /// Requests sent and not yet answered by a `Done`; true for a `Recheck`.
    in_flight: VecDeque<bool>,
}

impl Poller {
    pub fn new(now: Instant) -> Poller {
        Poller {
            next_scheduled: Some(now),
            recheck: None,
            rechecked: None,
            in_flight: VecDeque::new(),
        }
    }

    /// Requests due at `now`; each returned request must be sent.
    pub fn due(&mut self, now: Instant) -> Vec<FetchRequest> {
        let mut out = Vec::new();
        if self.next_scheduled.is_some_and(|at| now >= at) {
            self.next_scheduled = None;
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

    /// Called for every `Done`, with its cache's `fetched_at` and the app's unknown merge
    /// ids after applying it.
    pub fn on_done(
        &mut self,
        now: Instant,
        fetched_at: Option<DateTime<Utc>>,
        unknown: Vec<String>,
    ) {
        if self.in_flight.pop_front().unwrap_or(false) {
            return; // a recheck's answer never schedules another
        }
        self.next_scheduled = Some(now + POLL_EVERY);
        if unknown.is_empty() {
            self.recheck = None;
        } else if self.rechecked != Some(fetched_at) {
            self.rechecked = Some(fetched_at);
            self.recheck = Some((now + RECHECK_AFTER, unknown));
        }
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
        // A lock that cannot be tried (I/O error) must not stop fetching: treat it as ours.
        let (mut lock, have_lock) = match self.state.try_fetch_lock() {
            Ok(Some(file)) => (Some(file), true),
            Ok(None) => (None, false),
            Err(_) => (None, true),
        };
        let cache = self.load();
        let age = cache.fetched_at.map(|t| Utc::now() - t);
        match decide(req, age, have_lock) {
            Plan::UseCache => self.send(FetchEvent::Done(cache)),
            Plan::WaitForOther => {
                // The other copy releases the lock once its fetch is written; no gh calls here.
                drop(self.state.fetch_lock());
                self.send(FetchEvent::Done(self.load()))
            }
            Plan::Fetch => {
                self.send(FetchEvent::Started)?;
                if lock.is_none() {
                    // If locking fails, fetch without the lock rather than not at all.
                    lock = self.state.fetch_lock().ok();
                }
                let cache = match req {
                    // A failed recheck is dropped: the cache (and its error state) stays as is.
                    FetchRequest::Recheck(ids) => match self.recheck(ids) {
                        Ok(prs) => self.save(Ok(Change::Merge(prs))),
                        Err(_) => self.load(),
                    },
                    _ => {
                        let result = self.fetch_all()?;
                        self.save(result)
                    }
                };
                drop(lock);
                self.send(FetchEvent::Done(cache))
            }
        }
    }

    /// Steps 1-3 of spec §5; step 2 also covers every PR in `workspaces.json`. The outer
    /// error means the UI is gone (nothing is saved).
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
        // PRs any workspace remembers (spec §8), even merged or closed ones.
        let memory: Memory = self.state.load(workspaces::FILE).unwrap_or_default();
        for r in memory.all() {
            if seen.insert(r.pr_id.clone()) {
                by_repo.entry(r.repo.clone()).or_default().push(Target {
                    id: r.pr_id.clone(),
                    head_ref: Some(r.head_ref.clone()),
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

    fn recheck(&self, ids: &[String]) -> Result<Vec<Pr>, GhError> {
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
        detail_chunked(self.gh.as_ref(), &targets)
    }

    /// Records what changed in `changes.json`, then writes the outcome into `cache.json`
    /// (each under its lock, against the latest copy) and returns the cache as written. A
    /// cache write failure is reported in the returned cache.
    fn save(&self, result: Result<Change, GhError>) -> Cache {
        let now = Utc::now();
        // Still under the fetch lock: one copy records each change, once (spec §9). Timed
        // now, not at the fetch's start, so a visit that stamped meanwhile still marks it.
        // A failed write only loses this fetch's events.
        if let Ok(change) = &result {
            let prs = match change {
                Change::Full(c) => &c.prs,
                Change::Merge(prs) => prs,
            };
            let _ = self
                .state
                .update(CHANGES_FILE, |c: &mut Changes| c.observe(prs, now));
        }
        let apply = |c: &mut Cache| match &result {
            Ok(Change::Full(new)) => *c = new.clone(),
            // A recheck only refreshes its PRs; the error state belongs to the full fetch.
            Ok(Change::Merge(prs)) => merge_prs(&mut c.prs, prs.clone()),
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
    fn worker_fetches_remembered_ids() {
        use crate::workspaces::{self, Memory, Remembered};
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        state
            .update(workspaces::FILE, |m: &mut Memory| {
                for (repo, n, head) in [
                    ("heard/heard-ts", 1270, "audit/flags"),
                    ("heard/heard-ts", 1287, "audit/kafka-producer"),
                    ("heard/web", 7, "web/thing"),
                ] {
                    m.remember(
                        "/work/x",
                        Remembered {
                            repo: repo.into(),
                            number: n,
                            pr_id: format!("PR_{n}"),
                            head_ref: head.into(),
                        },
                    );
                }
                true
            })
            .unwrap();
        /// FakeGh's answers, with each call's variables copied out of the worker thread.
        struct Shared(
            FakeGh,
            std::sync::Arc<std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>>,
        );
        impl Gh for Shared {
            fn graphql(&self, q: &str, v: &serde_json::Value) -> Result<String, GhError> {
                let out = self.0.graphql(q, v);
                let last = self.0.calls.borrow().last().cloned().expect("recorded");
                self.1.lock().unwrap().push(last);
                out
            }
        }
        let log = std::sync::Arc::default();
        let gh = FakeGh::new()
            .with("search", Ok(SEARCH.into()))
            .with("detail", Ok(DETAIL.into()));
        let (tx, rx) = spawn(Box::new(Shared(gh, std::sync::Arc::clone(&log))), state);
        tx.send(FetchRequest::Forced).unwrap();
        until_done(&rx);
        let calls = log.lock().unwrap();
        let details: Vec<&serde_json::Value> = calls
            .iter()
            .filter(|(k, _)| *k == "detail")
            .map(|(_, v)| v)
            .collect();
        // One request per repo, remembered ids alongside the search hits.
        let find = |id: &str| {
            details.iter().find_map(|v| {
                let o = v.as_object().unwrap();
                let (key, _) = o.iter().find(|(_, val)| val.as_str() == Some(id))?;
                let n = key.strip_prefix("id").unwrap();
                Some((o.len(), o.get(&format!("head{n}")).cloned()))
            })
        };
        assert_eq!(
            find("PR_1270").map(|(_, h)| h),
            Some(Some(serde_json::json!("audit/flags")))
        );
        assert_eq!(
            find("PR_7").map(|(_, h)| h),
            Some(Some(serde_json::json!("web/thing")))
        );
        // #1287 came from search too: fetched once.
        let times = details
            .iter()
            .flat_map(|v| v.as_object().unwrap().values())
            .filter(|v| v.as_str() == Some("PR_1287"))
            .count();
        assert_eq!(times, 1);
        let web = details
            .iter()
            .find(|v| v.as_object().unwrap().values().any(|x| x == "PR_7"))
            .unwrap();
        assert_eq!(web.as_object().unwrap().len(), 2, "heard/web alone: {web}");
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

    /// Two full fetches (detail-before, then detail) and a recheck: `changes.json` holds
    /// board 6's changes, recorded by the worker that fetched.
    #[test]
    fn worker_records_changes() {
        use crate::changes::{CHANGES_FILE, Changes};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let before = include_str!("../tests/fixtures/detail-before.json");
        for (req, detail) in [
            (FetchRequest::Forced, before),
            (FetchRequest::Forced, DETAIL),
            (FetchRequest::Recheck(vec!["PR_1287".into()]), DETAIL),
        ] {
            let gh = FakeGh::new()
                .with("search", Ok(SEARCH.into()))
                .with("detail", Ok(detail.into()));
            let (tx, rx) = spawn(Box::new(gh), StateDir::new(dir.clone()));
            tx.send(req).unwrap();
            until_done(&rx);
        }
        let changes: Changes = StateDir::new(dir).load(CHANGES_FILE).unwrap();
        assert_eq!(changes.prs.len(), 9);
        let changed: Vec<&str> = changes
            .prs
            .iter()
            .filter(|(_, t)| t.changed_at.is_some())
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(changed, ["PR_1284", "PR_1287", "PR_1291", "PR_412"]);
        // The recheck saw nothing new: each change recorded once.
        assert_eq!(changes.prs["PR_1287"].events.len(), 4);
    }

    /// Changes are stamped when they are saved, not when the fetch started: a visit that
    /// stamps while a slow fetch runs must still see that fetch's changes as new.
    #[test]
    fn changes_are_timed_at_save() {
        use crate::changes::{CHANGES_FILE, Changes};
        struct Slow(FakeGh);
        impl Gh for Slow {
            fn graphql(&self, q: &str, v: &serde_json::Value) -> Result<String, GhError> {
                std::thread::sleep(std::time::Duration::from_millis(30));
                self.0.graphql(q, v)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let mut fetched_at = None;
        for detail in [include_str!("../tests/fixtures/detail-before.json"), DETAIL] {
            let gh = FakeGh::new()
                .with("search", Ok(SEARCH.into()))
                .with("detail", Ok(detail.into()));
            let (tx, rx) = spawn(Box::new(Slow(gh)), StateDir::new(dir.clone()));
            tx.send(FetchRequest::Forced).unwrap();
            let FetchEvent::Done(cache) = until_done(&rx).pop().unwrap() else {
                unreachable!()
            };
            fetched_at = cache.fetched_at;
        }
        let changes: Changes = StateDir::new(dir).load(CHANGES_FILE).unwrap();
        let at = changes.prs["PR_1287"].changed_at.unwrap();
        // Search, then two repos' details: at least 90 ms after the fetch started.
        assert!(
            at - fetched_at.unwrap() >= Duration::milliseconds(90),
            "{at} vs {fetched_at:?}"
        );
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
    fn poller_schedules_after_done_and_rechecks_once_per_fetch() {
        use FetchRequest::*;
        use std::time::Duration as D;
        let s = |n| D::from_secs(n);
        let f1: chrono::DateTime<Utc> = "2026-10-17T14:32:08Z".parse().unwrap();
        let (f2, f3) = (f1 + Duration::seconds(65), f1 + Duration::seconds(70));
        let a = || vec!["A".to_string()];
        let t0 = Instant::now();
        let mut p = Poller::new(t0);
        assert_eq!(p.due(t0), vec![Scheduled]);
        assert!(p.due(t0 + s(1)).is_empty());
        p.on_done(t0 + s(2), Some(f1), a());
        assert!(p.due(t0 + s(11)).is_empty());
        assert_eq!(p.due(t0 + s(12)), vec![Recheck(a())]);
        // The recheck's own Done does not schedule another, nor move the 60 s timer.
        p.on_done(t0 + s(13), Some(f1), a());
        assert!(p.due(t0 + s(61)).is_empty());
        assert_eq!(p.due(t0 + s(62)), vec![Scheduled]);
        // An answer carrying the same fetch (UseCache / WaitForOther) does not recheck again.
        p.on_done(t0 + s(63), Some(f1), a());
        assert!(p.due(t0 + s(122)).is_empty());
        assert_eq!(p.due(t0 + s(123)), vec![Scheduled]);
        // A new fetch with unknown ids rechecks once more...
        p.on_done(t0 + s(125), Some(f2), a());
        // ...unless a later fetch no longer has any.
        assert_eq!(p.forced(), Forced);
        p.on_done(t0 + s(128), Some(f3), vec![]);
        assert!(p.due(t0 + s(140)).is_empty());
        assert_eq!(p.due(t0 + s(188)), vec![Scheduled]);
    }

    #[test]
    fn next_scheduled_sees_cache_at_least_60s_old() {
        use std::time::Duration as D;
        let t0 = Instant::now();
        let mut p = Poller::new(t0);
        p.due(t0);
        // fetched_at is stamped when the fetch starts, before its Done arrives.
        let done_wall = Utc::now();
        let fetched_at = done_wall - Duration::seconds(3);
        let done = t0 + D::from_secs(5);
        p.on_done(done, Some(fetched_at), vec![]);
        assert!(p.due(done + D::from_millis(59_999)).is_empty());
        assert_eq!(
            p.due(done + D::from_secs(60)),
            vec![FetchRequest::Scheduled]
        );
        let age = (done_wall + Duration::seconds(60)) - fetched_at;
        assert!(age >= Duration::seconds(60));
        assert_eq!(
            decide(&FetchRequest::Scheduled, Some(age), true),
            Plan::Fetch
        );
        assert_eq!(
            decide(&FetchRequest::Scheduled, Some(Duration::seconds(60)), true),
            Plan::Fetch
        );
    }

    #[test]
    fn wait_for_other_waits_for_its_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let other = StateDir::new(path.clone());
        let lock = other.try_fetch_lock().unwrap().expect("free lock");
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            held_rx.recv().unwrap();
            thread::sleep(std::time::Duration::from_millis(200));
            other
                .update(CACHE, |c: &mut Cache| {
                    c.viewer = "other-copy".into();
                    c.fetched_at = Some(Utc::now());
                    true
                })
                .unwrap();
            drop(lock);
        });
        // No canned responses: any gh call would put an error into the cache.
        let (tx, rx) = spawn(Box::new(FakeGh::new()), StateDir::new(path));
        let start = Instant::now();
        tx.send(FetchRequest::Scheduled).unwrap();
        held_tx.send(()).unwrap();
        let evs = until_done(&rx);
        holder.join().unwrap();
        // Answered when the other copy lets go (~200 ms), not after a fixed sleep.
        assert!(start.elapsed() < std::time::Duration::from_millis(1500));
        assert_eq!(evs.len(), 1, "no Started: {evs:?}");
        let FetchEvent::Done(cache) = &evs[0] else {
            unreachable!()
        };
        assert_eq!(cache.viewer, "other-copy");
        assert_eq!(cache.error, None);
    }

    #[test]
    fn recheck_leaves_error_alone_and_drops_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let prs = crate::model::parse_detail(DETAIL).unwrap();
        let before: Cache = state
            .update(CACHE, |c: &mut Cache| {
                c.prs = prs.clone();
                c.error = Some(GhError::LoginExpired);
                c.error_at = Some(Utc::now());
                true
            })
            .unwrap();
        let ok = FakeGh::new().with("detail", Ok(DETAIL.into()));
        let (tx, rx) = spawn(Box::new(ok), StateDir::new(tmp.path().to_path_buf()));
        tx.send(FetchRequest::Recheck(vec![prs[0].id.clone()]))
            .unwrap();
        let FetchEvent::Done(after_ok) = until_done(&rx).pop().unwrap() else {
            unreachable!()
        };
        assert_eq!(after_ok.error, before.error);
        assert_eq!(after_ok.error_at, before.error_at);

        let failing = FakeGh::new().with("detail", Err(GhError::Other("boom".into())));
        let (tx, rx) = spawn(Box::new(failing), StateDir::new(tmp.path().to_path_buf()));
        tx.send(FetchRequest::Recheck(vec![prs[0].id.clone()]))
            .unwrap();
        let FetchEvent::Done(after_fail) = until_done(&rx).pop().unwrap() else {
            unreachable!()
        };
        assert_eq!(after_fail, after_ok);
        let on_disk: Cache = state.load(CACHE).unwrap();
        assert_eq!(on_disk, after_ok);
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
