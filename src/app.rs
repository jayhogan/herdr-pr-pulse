//! View model for the Mine view (spec §7, §10): turns PRs into a renderable `Screen`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};

use crate::attention::{
    AgentState, Attention, Counts, Reason, count, first_blocker, level, mergeable_through,
};
use crate::fetch::{FetchEvent, RepoStatus};
use crate::git::Branches;
use crate::herdr::Snapshot;
use crate::model::{MergeState, Pr, PrState};
use crate::stacks::{self, Group, GroupKind};
use crate::store::StateDir;
use crate::workspaces::{self, Memory, PaneCheckout, Remembered};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Workspace,
    Mine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Both,
    Authored,
    Assigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Urgency,
    Updated,
    Repo,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadProgress {
    pub repos: Vec<(String, RepoStatus)>,
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmptyInfo;
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Banner {
    pub message: String,
    pub fix: String,
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FilterState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDot {
    pub state: AgentState,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowView {
    pub pr: Pr,
    /// 1-based among open layers; 0 for merged/closed.
    pub layer: usize,
    pub is_last: bool,
    pub attention: Option<Attention>,
    pub waiting_on: Option<u32>,
    pub agent: Option<AgentDot>,
    pub checked_out: bool,
    pub changed: bool,
    pub change_lines: Vec<String>,
    pub foreign_author: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupView {
    pub key: String,
    pub repo: String,
    pub name: String,
    pub kind: GroupKind,
    pub base: String,
    pub folded: bool,
    pub rank: Attention,
    /// One level per open layer, bottom first.
    pub minimap: Vec<Attention>,
    pub open_layers: usize,
    pub mergeable_through: usize,
    pub blocker: Option<(usize, Reason)>,
    pub agent: Option<AgentDot>,
    pub rows: Vec<RowView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub view: ViewKind,
    pub scope: Scope,
    pub prs: usize,
    pub stacks: usize,
    pub repos: usize,
    pub sort: SortOrder,
    pub fetched_at: Option<DateTime<Utc>>,
    pub fetching: bool,
    pub stale: bool,
    pub branch: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Summary {
    pub counts: Counts,
    pub changed: usize,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Loading(LoadProgress),
    Empty(EmptyInfo),
    Groups(Vec<GroupView>),
}

#[derive(Debug, PartialEq)]
pub struct Screen {
    pub header: Header,
    pub summary: Summary,
    pub banner: Option<Banner>,
    pub body: Body,
    /// Index into selectable rows: group headers and PR rows of unfolded groups.
    pub cursor: usize,
    pub drawer_open: bool,
    pub help: bool,
    pub filter: Option<FilterState>,
    pub status: Option<String>,
    pub now: DateTime<Utc>,
    pub clock: FixedOffset,
}

struct Data {
    groups: Vec<Group>,
    authored: BTreeSet<String>,
    assigned: BTreeSet<String>,
    viewer: String,
    fetched_at: DateTime<Utc>,
}

pub struct App {
    kind: ViewKind,
    clock: FixedOffset,
    scope: Scope,
    sort: SortOrder,
    cursor: usize,
    folds: BTreeSet<String>,
    /// PR Pulse's own pane, for `⎇` and so a Workspace copy remembers its checkout.
    own_pane: Option<String>,
    /// The last herdr snapshot succeeded. When false there are no agents and memory is
    /// not pruned.
    herdr_ok: bool,
    panes: Vec<PaneCheckout>,
    memory: Memory,
    path_exists: fn(&str) -> bool,
    /// Per group key: the stack's agent and its checked-out layer.
    agents: BTreeMap<String, (AgentDot, Option<u32>)>,
    data: Option<Data>,
    fetching: bool,
    /// The last fetch failed; rows (if any) are from an earlier one.
    stale: bool,
    banner: Option<Banner>,
    progress: LoadProgress,
}

impl Scope {
    fn next(self) -> Scope {
        match self {
            Scope::Both => Scope::Authored,
            Scope::Authored => Scope::Assigned,
            Scope::Assigned => Scope::Both,
        }
    }
}

impl SortOrder {
    fn next(self) -> SortOrder {
        match self {
            SortOrder::Urgency => SortOrder::Updated,
            SortOrder::Updated => SortOrder::Repo,
            SortOrder::Repo => SortOrder::Urgency,
        }
    }
}

impl App {
    pub fn new(kind: ViewKind, clock: FixedOffset) -> App {
        App {
            kind,
            clock,
            scope: Scope::Both,
            sort: SortOrder::Urgency,
            cursor: 0,
            folds: BTreeSet::new(),
            own_pane: None,
            herdr_ok: false,
            panes: Vec::new(),
            memory: Memory::default(),
            path_exists: |p| Path::new(p).exists(),
            agents: BTreeMap::new(),
            data: None,
            fetching: false,
            stale: false,
            banner: None,
            progress: LoadProgress::default(),
        }
    }

    pub fn kind(&self) -> ViewKind {
        self.kind
    }

    pub fn set_prs(
        &mut self,
        prs: Vec<Pr>,
        authored: &[String],
        assigned: &[String],
        viewer: &str,
        fetched_at: DateTime<Utc>,
    ) {
        self.data = Some(Data {
            groups: stacks::group(&prs),
            authored: authored.iter().cloned().collect(),
            assigned: assigned.iter().cloned().collect(),
            viewer: viewer.to_string(),
            fetched_at,
        });
        self.refresh_agents();
    }

    pub fn apply_fetch(&mut self, ev: FetchEvent, now: DateTime<Utc>) {
        match ev {
            FetchEvent::Started => self.fetching = true,
            FetchEvent::Progress(repos) => self.progress = LoadProgress { repos },
            FetchEvent::Done(cache) => {
                self.fetching = false;
                self.progress = LoadProgress::default();
                if let Some(e) = &cache.error {
                    // Keep the rows we have; with none yet, show the last good fetch if any.
                    self.stale = true;
                    self.banner = Some(Banner {
                        message: e.message(),
                        fix: e.fix(),
                    });
                    if self.data.is_some() || cache.fetched_at.is_none() {
                        return;
                    }
                } else {
                    self.stale = false;
                    self.banner = None;
                    if cache.fetched_at.is_none() {
                        return; // nothing fetched yet (e.g. an empty cache): still loading
                    }
                }
                self.set_prs(
                    cache.prs,
                    &cache.authored,
                    &cache.assigned,
                    &cache.viewer,
                    cache.fetched_at.unwrap_or(now),
                );
            }
        }
    }

    /// Open PRs whose merge state GitHub had not computed yet.
    pub fn unknown_merge_ids(&self) -> Vec<String> {
        let Some(d) = &self.data else {
            return Vec::new();
        };
        d.groups
            .iter()
            .flat_map(|g| &g.layers)
            .filter(|p| p.state == PrState::Open && p.merge == MergeState::Unknown)
            .map(|p| p.id.clone())
            .collect()
    }

    pub fn cycle_scope(&mut self) {
        self.scope = self.scope.next();
        self.cursor = 0;
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.cursor = 0;
    }

    /// Moves the cursor by `delta` selectable rows, clamped to the first and last.
    pub fn move_cursor(&mut self, delta: isize) {
        let now = self.data.as_ref().map(|d| d.fetched_at).unwrap_or_default();
        let last = match self.screen(now).body {
            Body::Groups(gs) => gs
                .iter()
                .map(|g| 1 + if g.folded { 0 } else { g.rows.len() })
                .sum::<usize>()
                .saturating_sub(1),
            _ => 0,
        };
        self.cursor = self.cursor.min(last).saturating_add_signed(delta).min(last);
    }

    /// PR Pulse's own herdr pane (`$HERDR_PANE_ID`).
    pub fn set_own_pane(&mut self, pane_id: String) {
        self.own_pane = Some(pane_id);
    }

    /// How memory pruning checks that a checkout path still exists (fixtures fake it).
    pub fn set_path_exists(&mut self, exists: fn(&str) -> bool) {
        self.path_exists = exists;
    }

    /// Applies a herdr snapshot (spec §8): maps panes to checkouts, remembers the PR each
    /// pane has checked out, prunes the memory and recomputes the stacks' agents. An `Err`
    /// (herdr unreachable) clears the agents and leaves the memory alone.
    pub fn apply_snapshot(
        &mut self,
        snap: Result<Snapshot, String>,
        branches: &mut dyn Branches,
        state: &StateDir,
        _now: DateTime<Utc>,
    ) {
        let snap = match snap {
            Ok(s) => s,
            Err(_) => {
                self.herdr_ok = false;
                self.panes.clear();
                if let Ok(m) = state.load(workspaces::FILE) {
                    self.memory = m;
                }
                self.refresh_agents();
                return;
            }
        };
        self.herdr_ok = true;
        self.panes = workspaces::pane_checkouts(&snap, branches, self.own_pane.as_deref());

        let prs: Vec<&Pr> = self
            .data
            .iter()
            .flat_map(|d| d.groups.iter().flat_map(|g| &g.layers))
            .collect();
        let found: Vec<(String, Remembered)> = self
            .panes
            .iter()
            .filter_map(|pane| {
                // An open PR wins over an older merged/closed one on the same branch.
                let pr = prs
                    .iter()
                    .filter(|p| p.repo == pane.repo && p.head_ref == pane.branch)
                    .max_by_key(|p| p.state == PrState::Open)?;
                let r = Remembered {
                    repo: pr.repo.clone(),
                    number: pr.number,
                    pr_id: pr.id.clone(),
                    head_ref: pr.head_ref.clone(),
                };
                Some((pane.checkout_key.clone(), r))
            })
            .collect();
        let live: BTreeSet<String> = snap
            .workspaces
            .iter()
            .filter_map(|w| snap.checkout_key(&w.workspace_id))
            .chain(self.panes.iter().map(|p| p.checkout_key.clone()))
            .collect();
        let exists = self.path_exists;
        let change = |m: &mut Memory| {
            let mut changed = false;
            for (key, r) in &found {
                changed |= m.remember(key, r.clone());
            }
            changed | m.prune(Some(&live), exists)
        };
        match state.update(workspaces::FILE, change) {
            Ok(m) => self.memory = m,
            // Keep going on what we have; the next snapshot tries the file again.
            Err(_) => {
                change(&mut self.memory);
            }
        }
        self.refresh_agents();
    }

    fn refresh_agents(&mut self) {
        self.agents = match &self.data {
            Some(d) if self.herdr_ok => {
                workspaces::stack_agents(&d.groups, &self.panes, &self.memory)
            }
            _ => BTreeMap::new(),
        };
    }

    fn in_scope(&self, d: &Data, id: &str) -> bool {
        let authored = d.authored.contains(id);
        let assigned = d.assigned.contains(id);
        match self.scope {
            Scope::Both => authored || assigned,
            Scope::Authored => authored,
            Scope::Assigned => assigned,
        }
    }

    fn group_view(&self, d: &Data, g: &Group) -> GroupView {
        let (dot, checked_out) = match self.agents.get(&g.key) {
            Some((dot, n)) => (Some(dot.clone()), *n),
            None => (None, None),
        };
        // The group's attention follows its agent, wherever that agent's pane is.
        let agent_state = dot.as_ref().map(|d| d.state);
        let mut layer = 0;
        let last = g.layers.len().saturating_sub(1);
        let rows: Vec<RowView> = g
            .layers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let open = p.state == PrState::Open;
                // The dot shows once, on the checked-out layer (the renderer picks per view).
                let here = checked_out == Some(p.number);
                if open {
                    layer += 1;
                }
                RowView {
                    pr: p.clone(),
                    layer: if open { layer } else { 0 },
                    is_last: i == last,
                    attention: open.then(|| level(p, layer == 1, agent_state)),
                    waiting_on: if open { g.waiting_on(p.number) } else { None },
                    agent: if here { dot.clone() } else { None },
                    checked_out: here,
                    changed: false,
                    change_lines: Vec::new(),
                    foreign_author: (p.author != d.viewer).then(|| p.author.clone()),
                }
            })
            .collect();
        let minimap: Vec<Attention> = rows.iter().filter_map(|r| r.attention).collect();
        GroupView {
            key: g.key.clone(),
            repo: g.repo.clone(),
            name: g.name.clone(),
            kind: g.kind,
            base: g.base.clone(),
            folded: self.folds.contains(&g.key),
            rank: minimap.iter().copied().max().unwrap_or(Attention::Waiting),
            open_layers: minimap.len(),
            minimap,
            mergeable_through: mergeable_through(g),
            blocker: first_blocker(g),
            agent: dot,
            rows,
        }
    }

    /// The branch PR Pulse's own pane has checked out (header `⎇`).
    fn own_branch(&self) -> Option<String> {
        let own = self.own_pane.as_deref()?;
        self.panes
            .iter()
            .find(|p| p.pane_id == own)
            .map(|p| p.branch.clone())
    }

    pub fn screen(&self, now: DateTime<Utc>) -> Screen {
        let mut header = Header {
            view: self.kind,
            scope: self.scope,
            prs: 0,
            stacks: 0,
            repos: 0,
            sort: self.sort,
            fetched_at: None,
            fetching: self.fetching,
            stale: self.stale,
            branch: self.own_branch(),
        };
        let mut counts = Counts::default();
        let body = match &self.data {
            None => Body::Loading(self.progress.clone()),
            Some(d) => {
                header.fetched_at = Some(d.fetched_at);
                let mut shown: Vec<(GroupView, DateTime<Utc>)> = d
                    .groups
                    .iter()
                    .filter(|g| g.layers.iter().any(|p| self.in_scope(d, &p.id)))
                    .map(|g| {
                        let updated = g.layers.iter().map(|p| p.updated_at).max();
                        (self.group_view(d, g), updated.unwrap_or(now))
                    })
                    .collect();
                match self.sort {
                    SortOrder::Urgency => {
                        shown.sort_by(|a, b| b.0.rank.cmp(&a.0.rank).then(b.1.cmp(&a.1)));
                    }
                    SortOrder::Updated => shown.sort_by_key(|a| std::cmp::Reverse(a.1)),
                    SortOrder::Repo => {
                        shown.sort_by(|a, b| a.0.repo.cmp(&b.0.repo).then(b.1.cmp(&a.1)));
                    }
                }
                let in_scope_levels: Vec<Attention> = shown
                    .iter()
                    .flat_map(|(g, _)| &g.rows)
                    .filter(|r| self.in_scope(d, &r.pr.id))
                    .filter_map(|r| r.attention)
                    .collect();
                header.prs = in_scope_levels.len();
                counts = count(in_scope_levels);
                header.stacks = shown.len();
                header.repos = shown
                    .iter()
                    .map(|(g, _)| g.repo.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                Body::Groups(shown.into_iter().map(|(g, _)| g).collect())
            }
        };
        let selectable = match &body {
            Body::Groups(gs) => gs
                .iter()
                .map(|g| 1 + if g.folded { 0 } else { g.rows.len() })
                .sum(),
            _ => 0,
        };
        Screen {
            header,
            summary: Summary {
                counts,
                changed: 0,
                since: None,
            },
            banner: self.banner.clone(),
            body,
            cursor: self.cursor.min(selectable.saturating_sub(1)),
            drawer_open: false,
            help: false,
            filter: None,
            status: None,
            now,
            clock: self.clock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    fn now() -> DateTime<Utc> {
        "2026-10-17T14:32:26Z".parse().unwrap()
    }

    fn app() -> App {
        let prs = model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap();
        let s = model::parse_search(include_str!("../tests/fixtures/search.json")).unwrap();
        let ids = |h: &[model::SearchHit]| h.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.set_prs(
            prs,
            &ids(&s.authored),
            &ids(&s.assigned),
            &s.viewer,
            "2026-10-17T14:32:08Z".parse().unwrap(),
        );
        a
    }

    /// Agents from the fixture snapshot, seen from the Mine copy's pane (w4:p1).
    fn with_agents(a: &mut App, state: &StateDir) {
        a.set_own_pane("w4:p1".into());
        a.set_path_exists(|_| true);
        let snap = crate::herdr::parse_snapshot(include_str!("../tests/fixtures/snapshot.json"));
        a.apply_snapshot(
            snap,
            &mut crate::git::FixtureBranches::embedded(),
            state,
            now(),
        );
    }

    fn groups(a: &App) -> Vec<GroupView> {
        match a.screen(now()).body {
            Body::Groups(g) => g,
            other => panic!("expected groups, got {other:?}"),
        }
    }

    fn audit_key(a: &App) -> String {
        groups(a)
            .into_iter()
            .find(|g| g.name == "audit")
            .unwrap()
            .key
    }

    fn good_cache() -> crate::store::Cache {
        let s = model::parse_search(include_str!("../tests/fixtures/search.json")).unwrap();
        let ids = |h: &[model::SearchHit]| h.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        crate::store::Cache {
            version: 1,
            fetched_at: Some("2026-10-17T14:32:08Z".parse().unwrap()),
            viewer: s.viewer.clone(),
            authored: ids(&s.authored),
            assigned: ids(&s.assigned),
            prs: model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap(),
            error: None,
            error_at: None,
        }
    }

    #[test]
    fn apply_fetch_keeps_rows_on_error() {
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.apply_fetch(FetchEvent::Done(good_cache()), now());
        let s = a.screen(now());
        assert!(!s.header.stale);
        assert_eq!(s.banner, None);
        let Body::Groups(before) = s.body else {
            panic!()
        };

        a.apply_fetch(FetchEvent::Started, now());
        assert!(a.screen(now()).header.fetching);
        let bad = crate::store::Cache {
            prs: Vec::new(),
            error: Some(crate::github::GhError::LoginExpired),
            error_at: Some(now()),
            ..good_cache()
        };
        a.apply_fetch(FetchEvent::Done(bad), now());
        let s = a.screen(now());
        assert!(s.header.stale);
        assert!(!s.header.fetching);
        assert_eq!(
            s.banner,
            Some(Banner {
                message: "GitHub login expired".into(),
                fix: "Run `gh auth login` in any pane, then press r.".into(),
            })
        );
        assert_eq!(s.body, Body::Groups(before));

        // The next good fetch clears the error.
        a.apply_fetch(FetchEvent::Done(good_cache()), now());
        let s = a.screen(now());
        assert!(!s.header.stale);
        assert_eq!(s.banner, None);
    }

    #[test]
    fn empty_default_cache_keeps_loading() {
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.apply_fetch(FetchEvent::Started, now());
        a.apply_fetch(
            FetchEvent::Progress(vec![("heard/docket".into(), RepoStatus::Loading)]),
            now(),
        );
        a.apply_fetch(FetchEvent::Done(crate::store::Cache::default()), now());
        let s = a.screen(now());
        assert_eq!(s.body, Body::Loading(LoadProgress::default()));
        assert!(!s.header.fetching);
        assert!(a.unknown_merge_ids().is_empty());
    }

    #[test]
    fn progress_shows_only_before_data() {
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        let repos = vec![("heard/docket".to_string(), RepoStatus::Loading)];
        a.apply_fetch(FetchEvent::Progress(repos.clone()), now());
        assert_eq!(
            a.screen(now()).body,
            Body::Loading(LoadProgress {
                repos: repos.clone()
            })
        );
        a.apply_fetch(FetchEvent::Done(good_cache()), now());
        a.apply_fetch(FetchEvent::Progress(repos), now());
        assert!(matches!(a.screen(now()).body, Body::Groups(_)));
    }

    #[test]
    fn error_before_any_data_still_loading_with_banner() {
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        let bad = crate::store::Cache {
            error: Some(crate::github::GhError::NotInstalled),
            ..Default::default()
        };
        a.apply_fetch(FetchEvent::Done(bad), now());
        let s = a.screen(now());
        assert!(matches!(s.body, Body::Loading(_)));
        assert!(s.header.stale);
        assert_eq!(s.banner.unwrap().message, "gh not found");
    }

    #[test]
    fn unknown_merge_ids_lists_open_unknown() {
        let mut cache = good_cache();
        for p in &mut cache.prs {
            p.merge = model::MergeState::Clean;
        }
        cache.prs[0].merge = model::MergeState::Unknown;
        let first = cache.prs[0].id.clone();
        let merged = cache
            .prs
            .iter_mut()
            .find(|p| p.state != PrState::Open)
            .unwrap();
        merged.merge = model::MergeState::Unknown;
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        assert!(a.unknown_merge_ids().is_empty());
        a.apply_fetch(FetchEvent::Done(cache), now());
        assert_eq!(a.unknown_merge_ids(), vec![first]);
    }

    #[test]
    fn loading_before_data() {
        let a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        assert!(matches!(a.screen(now()).body, Body::Loading(_)));
    }

    #[test]
    fn mine_groups_in_urgency_order() {
        let a = app();
        let gs = groups(&a);
        assert_eq!(gs.len(), 4);
        assert_eq!(
            gs[0].rows.iter().map(|r| r.pr.number).collect::<Vec<_>>(),
            [1284, 1287, 1291, 1295]
        );
        assert_eq!(
            gs[1].rows.iter().map(|r| r.pr.number).collect::<Vec<_>>(),
            [412, 415]
        );
        assert_eq!(gs[2].rows[0].pr.number, 1279);
        assert_eq!(gs[3].rows[0].pr.number, 1302);
        assert_eq!(
            (gs[0].rank, gs[1].rank),
            (Attention::NeedsYou, Attention::NeedsYou)
        );
        assert_eq!(
            (gs[2].rank, gs[3].rank),
            (Attention::Nudge, Attention::Nudge)
        );
    }

    #[test]
    fn scope_cycles_and_filters() {
        let mut a = app();
        assert_eq!(a.screen(now()).header.scope, Scope::Both);
        a.cycle_scope();
        assert_eq!(a.screen(now()).header.scope, Scope::Authored);
        a.cycle_scope();
        let s = a.screen(now());
        assert_eq!(s.header.scope, Scope::Assigned);
        let Body::Groups(gs) = s.body else { panic!() };
        let nums: Vec<u32> = gs
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.pr.number))
            .collect();
        assert_eq!(nums, [412, 415, 1279]);
        assert_eq!(s.header.stacks, 2);
        assert_eq!(s.header.prs, 2);
        a.cycle_scope();
        assert_eq!(a.screen(now()).header.scope, Scope::Both);
    }

    #[test]
    fn summary_counts() {
        let mut a = app();
        let tmp = tempfile::tempdir().unwrap();
        with_agents(&mut a, &StateDir::new(tmp.path().to_path_buf()));
        let s = a.screen(now());
        assert_eq!(
            s.summary.counts,
            Counts {
                urgent: 1,
                needs_you: 1,
                nudge: 3,
                ready: 1,
                waiting: 2
            }
        );
        assert_eq!(s.header.prs, 8);
        assert_eq!(s.header.stacks, 4);
        assert_eq!(s.header.repos, 2);
    }

    #[test]
    fn agents_from_snapshot_and_dropped_when_herdr_unreachable() {
        let mut a = app();
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        with_agents(&mut a, &state);
        let key = audit_key(&a);
        let audit = |a: &App| groups(a).into_iter().find(|g| g.key == key).unwrap();
        let g = audit(&a);
        assert_eq!(
            g.agent,
            Some(AgentDot {
                state: AgentState::Idle,
                pane_id: "w1:p1".into()
            })
        );
        let row = g.rows.iter().find(|r| r.checked_out).unwrap();
        assert_eq!(row.pr.number, 1287);
        assert_eq!(row.agent, g.agent);
        assert_eq!(row.attention, Some(Attention::Urgent));
        assert!(g.rows.iter().filter(|r| r.agent.is_some()).count() == 1);

        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &state,
            now(),
        );
        let g = audit(&a);
        assert_eq!(g.agent, None);
        assert!(g.rows.iter().all(|r| r.agent.is_none() && !r.checked_out));
        assert_eq!(g.rank, Attention::NeedsYou);
    }

    #[test]
    fn upper_layer_waiting_on() {
        let a = app();
        let gs = groups(&a);
        let row = gs[0].rows.iter().find(|r| r.pr.number == 1291).unwrap();
        assert_eq!(row.waiting_on, Some(1287));
        assert_eq!(row.layer, 3);
        assert!(!row.is_last);
        assert!(gs[0].rows[3].is_last);
    }
}
