//! Change tracking (fingerprints, events) and per-view seen times.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::ViewKind;
use crate::model::{CiState, MergeState, Pr, PrState, ReviewState};

const MAX_EVENTS: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub ci: CiState,
    pub reviewers: BTreeMap<String, ReviewState>,
    pub threads: u32,
    pub merge: MergeState,
    pub behind: Option<u32>,
    pub head_oid: String,
    pub state: PrState,
}

impl Fingerprint {
    pub fn of(pr: &Pr) -> Fingerprint {
        Fingerprint {
            ci: pr.ci.state,
            reviewers: pr
                .reviewers
                .iter()
                .map(|r| (r.login.clone(), r.state))
                .collect(),
            threads: pr.unresolved_threads,
            merge: pr.merge,
            behind: pr.behind_by,
            head_oid: pr.head_oid.clone(),
            state: pr.state,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Changes {
    pub version: u32,
    pub prs: BTreeMap<String, Tracked>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tracked {
    pub fp: Fingerprint,
    pub changed_at: Option<DateTime<Utc>>,
    /// Newest last; at most 10.
    pub events: Vec<Event>,
}

impl Changes {
    /// Records the PRs' current fingerprints; true when anything stored changed.
    pub fn observe(&mut self, prs: &[Pr], now: DateTime<Utc>) -> bool {
        let mut dirty = false;
        for pr in prs {
            let fp = Fingerprint::of(pr);
            match self.prs.get_mut(&pr.id) {
                None => {
                    self.prs.insert(
                        pr.id.clone(),
                        Tracked {
                            fp,
                            changed_at: None,
                            events: Vec::new(),
                        },
                    );
                    dirty = true;
                }
                Some(t) if t.fp != fp => {
                    let texts = diff(&t.fp, pr);
                    if !texts.is_empty() {
                        t.changed_at = Some(now);
                        t.events
                            .extend(texts.into_iter().map(|text| Event { at: now, text }));
                        let excess = t.events.len().saturating_sub(MAX_EVENTS);
                        t.events.drain(..excess);
                    }
                    t.fp = fp;
                    dirty = true;
                }
                Some(_) => {}
            }
        }
        dirty
    }
}

fn is_clean(m: MergeState) -> bool {
    matches!(
        m,
        MergeState::Clean | MergeState::HasHooks | MergeState::Unstable
    )
}

/// Merge state moved into conflicts, or into clean from a non-clean state.
fn merge_event(old: &Fingerprint, pr: &Pr) -> bool {
    (pr.merge == MergeState::Dirty && old.merge != MergeState::Dirty)
        || (is_clean(pr.merge) && !is_clean(old.merge))
}

/// Human-readable events for what changed between `old` and the PR now.
pub fn diff(old: &Fingerprint, pr: &Pr) -> Vec<String> {
    let mut out = Vec::new();
    let ci = pr.ci.state;
    if ci != old.ci {
        match ci {
            CiState::Failing => {
                let name = pr.ci.failing.first().map(|c| c.name.as_str()).unwrap_or("");
                out.push(format!("✗ CI failed: {name}"));
            }
            CiState::Passing => out.push("✓ CI passed".into()),
            CiState::Running => out.push("◐ CI running".into()),
            CiState::None => {}
        }
    }
    let mut reviewers: Vec<_> = pr.reviewers.iter().collect();
    reviewers.sort_by_key(|r| (r.is_bot, r.login.as_str()));
    for r in reviewers {
        if old.reviewers.get(&r.login) == Some(&r.state) {
            continue;
        }
        let l = &r.login;
        match r.state {
            ReviewState::Approved => out.push(format!("✓ @{l} approved")),
            ReviewState::ChangesRequested => out.push(format!("✗ @{l} requested changes")),
            ReviewState::Commented => out.push(format!("◇ @{l} commented")),
            ReviewState::Pending => {}
        }
    }
    if pr.unresolved_threads > old.threads {
        out.push(format!(
            "◇ {} new thread(s)",
            pr.unresolved_threads - old.threads
        ));
    }
    let clean = |m| {
        matches!(
            m,
            MergeState::Clean | MergeState::HasHooks | MergeState::Unstable
        )
    };
    if pr.merge != old.merge {
        if pr.merge == MergeState::Dirty {
            out.push(format!("⚠ now conflicts with {}", pr.base_ref));
        } else if clean(pr.merge) && !clean(old.merge) {
            out.push("✓ merge clean".into());
        }
    }
    if let Some(n) = pr.behind_by {
        let into_behind = pr.merge == MergeState::Behind && old.merge != MergeState::Behind;
        if into_behind || n > old.behind.unwrap_or(0) {
            out.push(format!("↓ behind {} by {n}", pr.base_ref));
        }
    }
    if pr.head_oid != old.head_oid {
        out.push("↑ new commits pushed".into());
    }
    if pr.state != old.state {
        match pr.state {
            PrState::Merged => out.push("⊙ merged".into()),
            PrState::Closed => out.push("✕ closed".into()),
            PrState::Open => {}
        }
    }
    out
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Seen {
    pub version: u32,
    pub prs: BTreeMap<String, SeenAt>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SeenAt {
    pub ws: Option<DateTime<Utc>>,
    pub mine: Option<DateTime<Utc>>,
}

/// Workspace view: `ws`. Mine view: the later of `ws` and `mine` (None is oldest).
pub fn effective(seen: SeenAt, kind: ViewKind) -> Option<DateTime<Utc>> {
    match kind {
        ViewKind::Workspace => seen.ws,
        ViewKind::Mine => seen.ws.max(seen.mine),
    }
}

/// Stamps the given PRs as seen in this view; true when any stored time changed.
pub fn stamp(seen: &mut Seen, ids: &[String], kind: ViewKind, now: DateTime<Utc>) -> bool {
    let mut dirty = false;
    for id in ids {
        let s = seen.prs.entry(id.clone()).or_default();
        let slot = match kind {
            ViewKind::Workspace => &mut s.ws,
            ViewKind::Mine => &mut s.mine,
        };
        if *slot != Some(now) {
            *slot = Some(now);
            dirty = true;
        }
    }
    dirty
}

pub fn is_marked(t: &Tracked, baseline: Option<DateTime<Utc>>) -> bool {
    match (t.changed_at, baseline) {
        (Some(c), Some(b)) => c > b,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_detail;

    const DETAIL: &str = include_str!("../tests/fixtures/detail.json");
    const BEFORE: &str = include_str!("../tests/fixtures/detail-before.json");

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn marked(c: &Changes, n: u32, prs: &[Pr]) -> Vec<String> {
        let id = &prs.iter().find(|p| p.number == n).unwrap().id;
        let tr = &c.prs[id];
        if !is_marked(tr, None) {
            return vec![];
        }
        tr.events.iter().map(|e| e.text.clone()).collect()
    }

    #[test]
    fn board6_scenario() {
        let mut c = Changes::default();
        c.observe(&parse_detail(BEFORE).unwrap(), t("2026-10-17T14:18:00Z"));
        let after = parse_detail(DETAIL).unwrap();
        assert!(c.observe(&after, t("2026-10-17T14:32:08Z")));
        assert_eq!(
            marked(&c, 1287, &after),
            [
                "✗ CI failed: test-integration",
                "✗ @art requested changes",
                "◇ @claude[bot] commented",
                "◇ 3 new thread(s)"
            ]
        );
        assert_eq!(
            marked(&c, 1284, &after),
            ["✓ @kayla approved", "✓ merge clean"]
        );
        assert_eq!(marked(&c, 1291, &after), ["◇ 1 new thread(s)"]);
        assert_eq!(
            marked(&c, 412, &after),
            ["◇ 2 new thread(s)", "⚠ now conflicts with main"]
        );
        let changed: Vec<u32> = after
            .iter()
            .filter(|p| c.prs[&p.id].changed_at.is_some())
            .map(|p| p.number)
            .collect();
        assert_eq!(changed.len(), 4, "{changed:?}");
    }

    #[test]
    fn threads_decreasing_is_not_a_change() {
        let before = parse_detail(DETAIL).unwrap();
        let mut c = Changes::default();
        c.observe(&before, t("2026-10-17T14:00:00Z"));
        let mut after = before.clone();
        after[0].unresolved_threads = 0;
        c.observe(&after, t("2026-10-17T14:05:00Z"));
        assert!(c.prs[&after[0].id].changed_at.is_none());
        assert!(c.prs[&after[0].id].events.is_empty());
    }

    #[test]
    fn first_sight_is_not_a_change() {
        let prs = parse_detail(DETAIL).unwrap();
        let mut c = Changes::default();
        assert!(c.observe(&prs, t("2026-10-17T14:00:00Z")));
        assert!(
            c.prs
                .values()
                .all(|t| t.changed_at.is_none() && t.events.is_empty())
        );
        assert!(!c.observe(&prs, t("2026-10-17T14:01:00Z")));
    }

    #[test]
    fn keeps_at_most_ten_events() {
        let mut prs = parse_detail(DETAIL).unwrap();
        let mut c = Changes::default();
        c.observe(&prs, t("2026-10-17T14:00:00Z"));
        for i in 0..12 {
            prs[0].head_oid = format!("oid{i}");
            c.observe(&prs, t("2026-10-17T14:01:00Z"));
        }
        assert_eq!(c.prs[&prs[0].id].events.len(), 10);
    }

    #[test]
    fn effective_seen_rules() {
        let s = SeenAt {
            ws: Some(t("2026-10-17T10:00:00Z")),
            mine: Some(t("2026-10-17T11:00:00Z")),
        };
        assert_eq!(effective(s, ViewKind::Workspace), s.ws);
        assert_eq!(effective(s, ViewKind::Mine), s.mine);
        let s = SeenAt {
            ws: Some(t("2026-10-17T12:00:00Z")),
            mine: Some(t("2026-10-17T11:00:00Z")),
        };
        assert_eq!(effective(s, ViewKind::Mine), s.ws);
        let s = SeenAt {
            ws: None,
            mine: Some(t("2026-10-17T11:00:00Z")),
        };
        assert_eq!(effective(s, ViewKind::Mine), s.mine);
    }

    #[test]
    fn ws_stamp_clears_mine_but_not_reverse() {
        let tr = Tracked {
            fp: Fingerprint::of(&parse_detail(DETAIL).unwrap()[0]),
            changed_at: Some(t("2026-10-17T10:00:00Z")),
            events: vec![],
        };
        let mut seen = Seen::default();
        let ids = vec!["a".to_string()];
        assert!(stamp(
            &mut seen,
            &ids,
            ViewKind::Mine,
            t("2026-10-17T10:05:00Z")
        ));
        let s = seen.prs["a"];
        assert!(is_marked(&tr, effective(s, ViewKind::Workspace)));
        assert!(!is_marked(&tr, effective(s, ViewKind::Mine)));
        assert!(stamp(
            &mut seen,
            &ids,
            ViewKind::Workspace,
            t("2026-10-17T10:10:00Z")
        ));
        let s = seen.prs["a"];
        assert!(!is_marked(&tr, effective(s, ViewKind::Workspace)));
        assert!(!is_marked(&tr, effective(s, ViewKind::Mine)));
    }
}
