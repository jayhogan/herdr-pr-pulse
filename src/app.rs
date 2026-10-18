//! View model for the Mine view (spec §7, §10): turns PRs into a renderable `Screen`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset, Utc};

use crate::attention::{
    AgentState, Attention, Counts, Reason, count, first_blocker, level, mergeable_through,
};
use crate::fetch::{FetchEvent, RepoStatus};
use crate::model::{MergeState, Pr, PrState};
use crate::stacks::{self, Group, GroupKind};

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
    agents: BTreeMap<String, AgentState>,
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
            agents: BTreeMap::new(),
            data: None,
            fetching: false,
            stale: false,
            banner: None,
            progress: LoadProgress::default(),
        }
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

    /// Replaced by the real herdr mapping in Task 13.
    #[cfg(test)]
    pub fn set_agents_for_test(&mut self, agents: BTreeMap<String, AgentState>) {
        self.agents = agents;
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
        let agent_state = self.agents.get(&g.key).copied();
        let dot = agent_state.map(|state| AgentDot {
            state,
            pane_id: "test".into(),
        });
        let mut layer = 0;
        let last = g.layers.len().saturating_sub(1);
        let rows: Vec<RowView> = g
            .layers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let open = p.state == PrState::Open;
                if open {
                    layer += 1;
                }
                RowView {
                    pr: p.clone(),
                    layer: if open { layer } else { 0 },
                    is_last: i == last,
                    attention: open.then(|| level(p, layer == 1, agent_state)),
                    waiting_on: if open { g.waiting_on(p.number) } else { None },
                    agent: None,
                    checked_out: false,
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
            branch: None,
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
        let key = audit_key(&a);
        a.set_agents_for_test(BTreeMap::from([(key, AgentState::Idle)]));
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
