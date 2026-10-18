//! View model for the Mine view (spec §7, §10): turns PRs into a renderable `Screen`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ago;
use crate::attention::{
    AgentState, Attention, Counts, Reason, count, first_blocker, level, mergeable_through,
};
use crate::changes::{self, Changes, Seen};
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

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    /// Focus this herdr pane (the stack's agent).
    Focus(String),
    /// Open this PR URL in the browser.
    Open(String),
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
/// Why there is nothing to list (board 4).
#[derive(Debug, Clone, PartialEq)]
pub enum EmptyInfo {
    /// Nothing open in this scope; `other_scope_count` PRs are in the scope(s) `s` reaches.
    Mine {
        scope: Scope,
        other_scope_count: usize,
        repos: Vec<String>,
    },
    /// No stack in this workspace: its agent panes as (pane, branch, has PR, agent state).
    Workspace {
        panes: Vec<(String, String, bool, Option<AgentState>)>,
    },
    /// herdr is unreachable and this copy never learned which checkout it is in.
    HerdrDown,
    /// PR Pulse's own pane is not in a git checkout.
    NoCheckout,
}

/// Header and loading spinner frames, one per draw.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The banner while herdr does not answer (PR data stays live).
const HERDR_DOWN: &str = "herdr not responding · agent state ?";
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Banner {
    pub message: String,
    pub fix: String,
}
/// The `/` filter: `editing` while the text is being typed, applied after ⏎.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FilterState {
    pub text: String,
    pub editing: bool,
}

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
    /// `(k, N)` when this native-stack layer is Ready although GitHub marks it BLOCKED on its
    /// own: the stack merges as a whole.
    pub stack_ready: Option<(usize, usize)>,
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
    /// The spinner's current frame.
    pub spinner: &'static str,
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
    /// The PR whose drawer is open (when it is visible).
    pub drawer_pr: Option<String>,
    pub help: bool,
    /// First visible line of the help overlay.
    pub help_scroll: usize,
    pub filter: Option<FilterState>,
    pub status: Option<String>,
    /// The last herdr snapshot succeeded (else agent dots read `?`).
    pub herdr_ok: bool,
    /// The last herdr snapshot failed (not merely: none taken yet).
    pub herdr_down: bool,
    /// Workspace view: agent panes in this workspace, once herdr has answered.
    pub agent_panes: Option<usize>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    Group(String),
    Pr(String),
}

pub struct App {
    kind: ViewKind,
    clock: FixedOffset,
    scope: Scope,
    sort: SortOrder,
    cursor: usize,
    /// Id of the PR the drawer is attached to.
    drawer: Option<String>,
    /// The lone PR whose drawer was opened by itself, so a ␣ close sticks across refreshes.
    auto_drawer: Option<String>,
    help: bool,
    help_scroll: usize,
    /// How far the overlay can scroll, as last drawn.
    help_max: usize,
    folds: BTreeSet<String>,
    filter: Option<FilterState>,
    /// Where folds persist (`folds.json`); without one they live in memory only.
    state: Option<StateDir>,
    /// PR Pulse's own pane, for `⎇` and so a Workspace copy remembers its checkout.
    own_pane: Option<String>,
    /// The own pane's checkout key from the last good snapshot: whose memory the Workspace
    /// view shows. Kept while herdr is unreachable.
    own_checkout: Option<String>,
    /// One-line message shown in place of the key hints until the next key.
    status: Option<String>,
    /// The last herdr snapshot succeeded. When false there are no agents and memory is
    /// not pruned.
    herdr_ok: bool,
    /// The last herdr snapshot failed.
    herdr_down: bool,
    /// Failed herdr snapshots in a row; the second ends a visit.
    herdr_misses: u32,
    /// Spinner frame, advanced once per draw.
    spin: usize,
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
    /// Fingerprints and events (`changes.json`, written by the fetch worker).
    changes: Changes,
    /// Seen times (`seen.json`) as last read or written.
    seen: Seen,
    /// The current visit: this copy's pane is visible (spec §9).
    visit: Option<Visit>,
}

/// A visit (spec §9): from the own pane becoming visible until it is hidden again.
#[derive(Debug, Clone, PartialEq)]
pub struct Visit {
    pub started: DateTime<Utc>,
    /// Per PR on screen: this view's effective seen time from before the visit.
    pub baseline: BTreeMap<String, Option<DateTime<Utc>>>,
}

/// `folds.json`: a `BTreeSet<String>` of folded group keys.
const FOLDS_FILE: &str = "folds.json";

/// Whether group `g` matches the filter text: case-insensitive substring of its repo, any
/// `#number` or any title.
fn matches_filter(g: &Group, text: &str) -> bool {
    let q = text.to_lowercase();
    g.repo.to_lowercase().contains(&q)
        || g.layers
            .iter()
            .any(|p| format!("#{}", p.number).contains(&q) || p.title.to_lowercase().contains(&q))
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
            drawer: None,
            auto_drawer: None,
            help: false,
            help_scroll: 0,
            help_max: usize::MAX,
            folds: BTreeSet::new(),
            filter: None,
            state: None,
            own_pane: None,
            own_checkout: None,
            status: None,
            herdr_ok: false,
            herdr_down: false,
            herdr_misses: 0,
            spin: 0,
            panes: Vec::new(),
            memory: Memory::default(),
            path_exists: |p| Path::new(p).exists(),
            agents: BTreeMap::new(),
            data: None,
            fetching: false,
            stale: false,
            banner: None,
            progress: LoadProgress::default(),
            changes: Changes::default(),
            seen: Seen::default(),
            visit: None,
        }
    }

    /// Whether this copy's pane can be seen (spec §9). Becoming visible starts a visit:
    /// each PR on screen gets its baseline and is stamped seen. While visible, PRs that
    /// appear are baselined and stamped as they do. Hidden ends the visit.
    pub fn set_visible(&mut self, visible: bool, now: DateTime<Utc>) {
        if !visible {
            self.visit = None;
            return;
        }
        let start = self.visit.is_none();
        if start {
            // Changes another copy recorded since our last Done count from this visit on.
            self.reload_changes();
            self.visit = Some(Visit {
                started: now,
                baseline: BTreeMap::new(),
            });
        }
        self.see(now, start);
    }

    /// During a visit, stamps the PRs on screen as seen at `now` (all of them when `all`,
    /// else those new to the visit). A PR new to the visit first gets its baseline from
    /// the seen times stored before this stamp.
    fn see(&mut self, now: DateTime<Utc>, all: bool) {
        if self.visit.is_none() {
            return;
        }
        let ids = self.shown_ids();
        let kind = self.kind;
        let visit = self.visit.as_mut().expect("checked above");
        let fresh: Vec<String> = ids
            .iter()
            .filter(|id| !visit.baseline.contains_key(*id))
            .cloned()
            .collect();
        let stamped = if all { ids } else { fresh.clone() };
        if stamped.is_empty() {
            return;
        }
        let mut apply = |seen: &mut Seen| {
            for id in &fresh {
                let before = seen.prs.get(id).copied().unwrap_or_default();
                // Set once: after a failed write this runs again over the older seen
                // times in memory, which must not replace those read from the file.
                visit
                    .baseline
                    .entry(id.clone())
                    .or_insert(changes::effective(before, kind));
            }
            changes::stamp(seen, &stamped, kind, now)
        };
        match self
            .state
            .as_ref()
            .map(|s| s.update(changes::SEEN_FILE, &mut apply))
        {
            Some(Ok(seen)) => self.seen = seen,
            Some(Err(e)) => {
                apply(&mut self.seen);
                self.status = Some(format!("seen not saved: {e}"));
            }
            None => {
                apply(&mut self.seen);
            }
        }
    }

    /// `changes.json` as the fetch workers left it (kept as is when it cannot be read).
    fn reload_changes(&mut self) {
        if let Some(state) = &self.state
            && let Ok(c) = state.load(changes::CHANGES_FILE)
        {
            self.changes = c;
        }
    }

    /// Every PR whose row the view lists: not in a folded group, nor hidden by the filter.
    /// A group unfolded or unfiltered during a visit is stamped at the next snapshot.
    fn shown_ids(&self) -> Vec<String> {
        let Some(d) = &self.data else {
            return Vec::new();
        };
        if self.kind == ViewKind::Workspace && self.own_checkout.is_none() {
            return Vec::new();
        }
        let filter = self.filter.as_ref().filter(|f| !f.text.is_empty());
        d.groups
            .iter()
            .filter(|g| self.shows(d, g) && !self.folds.contains(&g.key))
            .filter(|g| filter.is_none_or(|f| matches_filter(g, &f.text)))
            .flat_map(|g| &g.layers)
            .map(|p| p.id.clone())
            .collect()
    }

    /// The seen time a PR's changes are compared against: from before the visit, or (no
    /// visit, or not yet baselined) the stored one.
    fn baseline(&self, id: &str) -> Option<DateTime<Utc>> {
        match self.visit.as_ref().and_then(|v| v.baseline.get(id)) {
            Some(b) => *b,
            None => {
                let seen = self.seen.prs.get(id).copied().unwrap_or_default();
                changes::effective(seen, self.kind)
            }
        }
    }

    /// Whether PR `id` changed since this view last saw it, and what changed, newest first,
    /// as "{event} {age} ago".
    fn change_marks(&self, id: &str, now: DateTime<Utc>) -> (bool, Vec<String>) {
        let Some(t) = self.changes.prs.get(id) else {
            return (false, Vec::new());
        };
        let base = self.baseline(id);
        if !changes::is_marked(t, base) {
            return (false, Vec::new());
        }
        let mut events: Vec<&changes::Event> = t
            .events
            .iter()
            .filter(|e| base.is_none_or(|b| e.at > b))
            .collect();
        // Stable: events of one fetch keep their order.
        events.sort_by_key(|e| std::cmp::Reverse(e.at));
        let lines = events
            .iter()
            .map(|e| format!("{} {} ago", e.text, ago::short(e.at, now)))
            .collect();
        (true, lines)
    }

    /// Keeps folds in `state`'s `folds.json` (loaded now, saved on every change), and seen
    /// times and changes in its `seen.json` and `changes.json`.
    pub fn with_state(mut self, state: StateDir) -> App {
        match state.load::<BTreeSet<String>>(FOLDS_FILE) {
            Ok(folds) => self.folds = folds,
            Err(e) => self.status = Some(format!("folds not loaded: {e}")),
        }
        self.changes = state.load(changes::CHANGES_FILE).unwrap_or_default();
        self.seen = state.load(changes::SEEN_FILE).unwrap_or_default();
        self.state = Some(state);
        self
    }

    /// Folds (or unfolds) the group under the cursor and keeps the cursor on its header.
    fn set_fold(&mut self, fold: bool) {
        let Some(key) = self.selected_group().map(|g| g.key) else {
            return;
        };
        if self.folds.contains(&key) == fold {
            return;
        }
        let apply = |folds: &mut BTreeSet<String>| {
            if fold {
                folds.insert(key.clone())
            } else {
                folds.remove(&key)
            }
        };
        match self.state.as_ref().map(|s| s.update(FOLDS_FILE, apply)) {
            Some(Ok(folds)) => self.folds = folds,
            Some(Err(e)) => {
                apply(&mut self.folds);
                self.status = Some(format!("folds not saved: {e}"));
            }
            None => {
                apply(&mut self.folds);
            }
        }
        self.restore(Some(Selection::Group(key)));
    }

    /// Keys while the filter text is being typed: everything but Ctrl-C is text input.
    fn edit_filter(&mut self, key: KeyEvent) -> Action {
        let Some(f) = self.filter.as_mut() else {
            return Action::None;
        };
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Quit;
            }
            KeyCode::Char(c) => f.text.push(c),
            KeyCode::Backspace => {
                f.text.pop();
            }
            KeyCode::Enter => f.editing = false,
            KeyCode::Esc => self.filter = None,
            _ => return Action::None,
        }
        self.cursor = 0;
        Action::None
    }

    /// Applies a key press. Every key clears the status line first.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.status = None;
        if self.filter.as_ref().is_some_and(|f| f.editing) {
            return self.edit_filter(key);
        }
        if self.help {
            return self.help_key(key);
        }
        match key.code {
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('/') => {
                self.filter.get_or_insert_with(FilterState::default).editing = true;
            }
            KeyCode::Esc if self.filter.is_some() => {
                self.filter = None;
                self.cursor = 0;
            }
            KeyCode::Char('h') | KeyCode::Left => self.set_fold(true),
            KeyCode::Char('l') | KeyCode::Right => self.set_fold(false),
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Quit;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char(' ') => self.toggle_drawer(),
            KeyCode::Char('r') => return Action::Refresh,
            KeyCode::Char('s') if self.kind == ViewKind::Mine => self.cycle_scope(),
            KeyCode::Char('S') => self.cycle_sort(),
            KeyCode::Enter => return self.jump(),
            KeyCode::Char('o') => {
                if let Some(pr) = self.selected_pr() {
                    return Action::Open(pr.url);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Keys while the help overlay is open: ?/Esc close it, j/k scroll it, q quits, the rest
    /// are ignored.
    fn help_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('?') | KeyCode::Esc => self.help = false,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Quit;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll = (self.help_scroll + 1).min(self.help_max);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1)
            }
            _ => {}
        }
        Action::None
    }

    /// The renderer reports how far the help overlay can scroll; keep the offset within it.
    pub fn set_help_max(&mut self, max: usize) {
        self.help_max = max;
        self.help_scroll = self.help_scroll.min(max);
    }

    /// ␣: open the drawer under the selected PR row, or close it; nothing on a group header.
    fn toggle_drawer(&mut self) {
        let Some((_, Some(row))) = self.selected() else {
            return;
        };
        if self.drawer.as_deref() == Some(row.pr.id.as_str()) {
            self.drawer = None;
        } else {
            self.drawer = Some(row.pr.id);
        }
    }

    /// In the Workspace view, a workspace with exactly one open PR opens that PR's drawer
    /// and puts the cursor on it — once per PR, so closing it with ␣ sticks.
    fn auto_open_drawer(&mut self) {
        if self.kind != ViewKind::Workspace {
            return;
        }
        let Body::Groups(gs) = self.screen(self.data_now()).body else {
            return;
        };
        let mut open = gs
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.pr.state == PrState::Open);
        let lone = match (open.next(), open.next()) {
            (Some(r), None) => Some(r.pr.id.clone()),
            _ => None,
        };
        if lone.is_some() && lone != self.auto_drawer {
            self.drawer = lone.clone();
            self.restore(lone.clone().map(Selection::Pr));
        }
        self.auto_drawer = lone;
    }

    /// ⏎: focus the selected stack's agent pane (spec §8).
    fn jump(&mut self) -> Action {
        if !self.herdr_ok {
            self.status = Some("herdr unreachable".into());
            return Action::None;
        }
        match self.selected_group().and_then(|g| g.agent) {
            Some(dot) => Action::Focus(dot.pane_id),
            None => {
                self.status = Some("no agent pane for this stack".into());
                Action::None
            }
        }
    }

    /// Turns the spinner one frame; the event loop calls this once per draw.
    pub fn advance_spinner(&mut self) {
        self.spin = (self.spin + 1) % SPINNER.len();
    }

    /// A one-line message in place of the key hints until the next key.
    pub fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    /// The group under the cursor (its header or one of its rows) and the row, if any.
    fn selected(&self) -> Option<(GroupView, Option<RowView>)> {
        let screen = self.screen(self.data_now());
        let Body::Groups(gs) = screen.body else {
            return None;
        };
        let mut i = 0;
        for g in gs {
            if i == screen.cursor {
                return Some((g, None));
            }
            i += 1;
            let rows = if g.folded { 0 } else { g.rows.len() };
            if screen.cursor < i + rows {
                let row = g.rows[screen.cursor - i].clone();
                return Some((g, Some(row)));
            }
            i += rows;
        }
        None
    }

    pub fn selected_group(&self) -> Option<GroupView> {
        self.selected().map(|(g, _)| g)
    }

    /// The PR under the cursor; on a group header, the stack's first row.
    pub fn selected_pr(&self) -> Option<Pr> {
        let (g, row) = self.selected()?;
        row.or_else(|| g.rows.into_iter().next()).map(|r| r.pr)
    }

    /// What the cursor is on, to find it again after the rows change.
    fn selection(&self) -> Option<Selection> {
        let (g, row) = self.selected()?;
        Some(match row {
            Some(r) => Selection::Pr(r.pr.id),
            None => Selection::Group(g.key),
        })
    }

    /// Puts the cursor back on `sel`; if it is gone, keeps the index (clamped on draw).
    fn restore(&mut self, sel: Option<Selection>) {
        let Some(sel) = sel else { return };
        let Body::Groups(gs) = self.screen(self.data_now()).body else {
            return;
        };
        let mut i = 0;
        for g in gs {
            if sel == Selection::Group(g.key.clone()) {
                self.cursor = i;
                return;
            }
            i += 1;
            if g.folded {
                continue;
            }
            for r in &g.rows {
                if sel == Selection::Pr(r.pr.id.clone()) {
                    self.cursor = i;
                    return;
                }
                i += 1;
            }
        }
    }

    /// A stable "now" for cursor bookkeeping (only the sort's fallback uses it).
    fn data_now(&self) -> DateTime<Utc> {
        self.data.as_ref().map(|d| d.fetched_at).unwrap_or_default()
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
        let sel = self.selection();
        self.data = Some(Data {
            groups: stacks::group(&prs),
            authored: authored.iter().cloned().collect(),
            assigned: assigned.iter().cloned().collect(),
            viewer: viewer.to_string(),
            fetched_at,
        });
        self.refresh_agents();
        self.restore(sel);
        // The drawer goes when its PR does.
        if let Some(id) = &self.drawer
            && !self
                .data
                .as_ref()
                .is_some_and(|d| d.groups.iter().flat_map(|g| &g.layers).any(|p| p.id == *id))
        {
            self.drawer = None;
        }
        self.auto_open_drawer();
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
                // The worker recorded this fetch's changes before writing the cache.
                self.reload_changes();
                self.set_prs(
                    cache.prs,
                    &cache.authored,
                    &cache.assigned,
                    &cache.viewer,
                    cache.fetched_at.unwrap_or(now),
                );
                // What is on screen during a visit has now been seen.
                self.see(now, true);
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
        let last = match self.screen(self.data_now()).body {
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
    /// (herdr unreachable) clears the agents and leaves the memory alone. Whether the own
    /// pane is visible starts or ends a visit (spec §9). Unreachable twice in a row counts
    /// as hidden; a single failure leaves the visit as it is (a blip must not drop marks).
    pub fn apply_snapshot(
        &mut self,
        snap: Result<Snapshot, String>,
        branches: &mut dyn Branches,
        state: &StateDir,
        now: DateTime<Utc>,
    ) {
        let visible = match (&snap, &self.own_pane) {
            (Ok(s), Some(own)) => Some(s.is_visible(own)),
            (Ok(_), None) => Some(false),
            (Err(_), _) => None,
        };
        self.herdr_misses = match visible {
            Some(_) => 0,
            None => self.herdr_misses.saturating_add(1),
        };
        let sel = self.selection();
        self.read_snapshot(snap, branches, state);
        self.refresh_agents();
        self.restore(sel);
        self.auto_open_drawer();
        match visible {
            Some(v) => self.set_visible(v, now),
            None if self.herdr_misses >= 2 => self.set_visible(false, now),
            None => {}
        }
    }

    fn read_snapshot(
        &mut self,
        snap: Result<Snapshot, String>,
        branches: &mut dyn Branches,
        state: &StateDir,
    ) {
        let snap = match snap {
            Ok(s) => s,
            Err(_) => {
                self.herdr_ok = false;
                self.herdr_down = true;
                self.panes.clear();
                if let Ok(m) = state.load(workspaces::FILE) {
                    self.memory = m;
                }
                return;
            }
        };
        self.herdr_ok = true;
        self.herdr_down = false;
        self.panes = workspaces::pane_checkouts(&snap, branches, self.own_pane.as_deref());
        if let Some(own) = self.own_pane() {
            self.own_checkout = Some(own.checkout_key.clone());
        }

        let prs: Vec<&Pr> = self
            .data
            .iter()
            .flat_map(|d| d.groups.iter().flat_map(|g| &g.layers))
            .collect();
        // Only agent panes: the own pane is there just for the header's ⎇.
        let found: Vec<(String, Remembered)> = self
            .panes
            .iter()
            .filter(|pane| pane.agent.is_some())
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
        // A snapshot listing no workspaces at all says nothing about which are in use.
        let live: Option<BTreeSet<String>> =
            (!snap.workspaces.is_empty()).then(|| workspaces::live_checkouts(&snap, branches));
        let exists = self.path_exists;
        let change = |m: &mut Memory| {
            let mut changed = false;
            for (key, r) in &found {
                changed |= m.remember(key, r.clone());
            }
            changed | m.prune(live.as_ref(), exists)
        };
        match state.update(workspaces::FILE, change) {
            Ok(m) => self.memory = m,
            // Keep going on what we have; the next snapshot tries the file again.
            Err(_) => {
                change(&mut self.memory);
            }
        }
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

    fn group_view(&self, d: &Data, g: &Group, now: DateTime<Utc>) -> GroupView {
        let (dot, checked_out) = match self.agents.get(&g.key) {
            Some((dot, n)) => (Some(dot.clone()), *n),
            None => (None, None),
        };
        // The group's attention follows its agent, wherever that agent's pane is.
        let agent_state = dot.as_ref().map(|d| d.state);
        let mut layer = 0;
        let through = mergeable_through(g);
        let open_total = g.open_layers().count();
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
                let (changed, change_lines) = self.change_marks(&p.id, now);
                let attention = open.then(|| level(p, layer == 1, agent_state));
                RowView {
                    pr: p.clone(),
                    layer: if open { layer } else { 0 },
                    is_last: i == last,
                    attention,
                    stack_ready: (attention == Some(Attention::Ready)
                        && p.merge == MergeState::Blocked)
                        .then_some((through, open_total)),
                    waiting_on: if open { g.waiting_on(p.number) } else { None },
                    agent: if here { dot.clone() } else { None },
                    checked_out: here,
                    changed,
                    change_lines,
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
            mergeable_through: through,
            blocker: first_blocker(g),
            agent: dot,
            rows,
        }
    }

    fn own_pane(&self) -> Option<&PaneCheckout> {
        let own = self.own_pane.as_deref()?;
        self.panes.iter().find(|p| p.pane_id == own)
    }

    /// Header `⎇`: the focused pane's branch when that pane is in this workspace, else the
    /// branch PR Pulse's own pane has checked out.
    fn header_branch(&self) -> Option<String> {
        let own = self.own_pane()?;
        let focused = self
            .panes
            .iter()
            .find(|p| p.focused && p.workspace_id == own.workspace_id);
        Some(focused.unwrap_or(own).branch.clone())
    }

    /// Whether the view lists group `g`: Mine by scope, Workspace by this checkout's memory
    /// (or the branch PR Pulse's pane has checked out right now).
    fn shows(&self, d: &Data, g: &Group) -> bool {
        match self.kind {
            ViewKind::Mine => g.layers.iter().any(|p| self.in_scope(d, &p.id)),
            ViewKind::Workspace => {
                let ids = self
                    .own_checkout
                    .as_deref()
                    .map(|k| self.memory.ids_for(k))
                    .unwrap_or_default();
                let own = self.own_pane();
                g.layers.iter().any(|p| {
                    ids.contains(&p.id)
                        || own.is_some_and(|o| o.repo == p.repo && o.branch == p.head_ref)
                })
            }
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
            branch: self.header_branch(),
            spinner: SPINNER[self.spin],
        };
        header.fetched_at = self.data.as_ref().map(|d| d.fetched_at);
        let mut summary = Summary {
            counts: Counts::default(),
            changed: 0,
            since: None,
        };
        // A Workspace copy that cannot tell which checkout it is in has nothing to list.
        let lost = match self.kind {
            ViewKind::Workspace if self.own_checkout.is_none() => {
                if self.herdr_down {
                    Some(EmptyInfo::HerdrDown)
                } else if self.herdr_ok {
                    Some(EmptyInfo::NoCheckout)
                } else {
                    None // no snapshot yet
                }
            }
            _ => None,
        };
        let body = match (&self.data, lost) {
            (_, Some(info)) => Body::Empty(info),
            (None, None) => Body::Loading(self.progress.clone()),
            (Some(d), None) => {
                let mut shown: Vec<(GroupView, DateTime<Utc>)> = d
                    .groups
                    .iter()
                    .filter(|g| self.shows(d, g))
                    .map(|g| {
                        let updated = g.layers.iter().map(|p| p.updated_at).max();
                        (self.group_view(d, g, now), updated.unwrap_or(now))
                    })
                    .collect();
                // Summary counts cover everything in scope; the filter only narrows the list.
                let all_levels: Vec<Attention> = shown
                    .iter()
                    .flat_map(|(g, _)| &g.rows)
                    .filter(|r| self.in_scope(d, &r.pr.id))
                    .filter_map(|r| r.attention)
                    .collect();
                summary.counts = count(all_levels);
                // ✦: every marked row listed; since = the oldest baseline among them.
                let marked = shown
                    .iter()
                    .flat_map(|(g, _)| &g.rows)
                    .filter(|r| r.changed);
                summary.changed = marked.clone().count();
                summary.since = marked.filter_map(|r| self.baseline(&r.pr.id)).min();
                if shown.is_empty() {
                    let body = Body::Empty(self.empty_info(d));
                    return self.finish_screen(header, summary, body, now);
                }
                if let Some(f) = self.filter.as_ref().filter(|f| !f.text.is_empty()) {
                    let keep: BTreeSet<&str> = d
                        .groups
                        .iter()
                        .filter(|g| matches_filter(g, &f.text))
                        .map(|g| g.key.as_str())
                        .collect();
                    shown.retain(|(g, _)| keep.contains(g.key.as_str()));
                }
                match self.sort {
                    SortOrder::Urgency => {
                        shown.sort_by(|a, b| b.0.rank.cmp(&a.0.rank).then(b.1.cmp(&a.1)));
                    }
                    SortOrder::Updated => shown.sort_by_key(|a| std::cmp::Reverse(a.1)),
                    SortOrder::Repo => {
                        shown.sort_by(|a, b| {
                            (
                                a.0.repo.as_str(),
                                std::cmp::Reverse(a.0.rank),
                                std::cmp::Reverse(a.1),
                            )
                                .cmp(&(
                                    b.0.repo.as_str(),
                                    std::cmp::Reverse(b.0.rank),
                                    std::cmp::Reverse(b.1),
                                ))
                        });
                    }
                }
                let in_scope_levels: Vec<Attention> = shown
                    .iter()
                    .flat_map(|(g, _)| &g.rows)
                    .filter(|r| self.in_scope(d, &r.pr.id))
                    .filter_map(|r| r.attention)
                    .collect();
                header.prs = in_scope_levels.len();
                header.stacks = shown.len();
                header.repos = shown
                    .iter()
                    .map(|(g, _)| g.repo.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                Body::Groups(shown.into_iter().map(|(g, _)| g).collect())
            }
        };
        self.finish_screen(header, summary, body, now)
    }

    /// What an empty view says instead (board 4).
    fn empty_info(&self, d: &Data) -> EmptyInfo {
        match self.kind {
            ViewKind::Mine => {
                let open = d
                    .groups
                    .iter()
                    .flat_map(|g| &g.layers)
                    .filter(|p| p.state == PrState::Open);
                let other_scope_count = open
                    .filter(|p| {
                        (d.authored.contains(&p.id) || d.assigned.contains(&p.id))
                            && !self.in_scope(d, &p.id)
                    })
                    .count();
                let repos: BTreeSet<String> = d.groups.iter().map(|g| g.repo.clone()).collect();
                EmptyInfo::Mine {
                    scope: self.scope,
                    other_scope_count,
                    repos: repos.into_iter().collect(),
                }
            }
            ViewKind::Workspace => {
                let panes = self
                    .workspace_agent_panes()
                    .into_iter()
                    .map(|p| {
                        let has_pr = d
                            .groups
                            .iter()
                            .flat_map(|g| &g.layers)
                            .any(|pr| pr.repo == p.repo && pr.head_ref == p.branch);
                        (p.pane_id.clone(), p.branch.clone(), has_pr, p.agent)
                    })
                    .collect();
                EmptyInfo::Workspace { panes }
            }
        }
    }

    /// Agent panes in PR Pulse's own workspace.
    fn workspace_agent_panes(&self) -> Vec<&PaneCheckout> {
        let Some(own) = self.own_pane() else {
            return Vec::new();
        };
        self.panes
            .iter()
            .filter(|p| p.agent.is_some() && p.workspace_id == own.workspace_id)
            .collect()
    }

    fn finish_screen(
        &self,
        header: Header,
        summary: Summary,
        body: Body,
        now: DateTime<Utc>,
    ) -> Screen {
        let selectable = match &body {
            Body::Groups(gs) => gs
                .iter()
                .map(|g| 1 + if g.folded { 0 } else { g.rows.len() })
                .sum(),
            _ => 0,
        };
        let drawer_pr = match &body {
            Body::Groups(gs) => self.drawer.clone().filter(|id| {
                gs.iter()
                    .filter(|g| !g.folded)
                    .flat_map(|g| &g.rows)
                    .any(|r| r.pr.id == *id)
            }),
            _ => None,
        };
        Screen {
            header,
            summary,
            // A GitHub error says more (and has a fix); herdr's shows when it is the only one.
            banner: self.banner.clone().or_else(|| {
                self.herdr_down.then(|| Banner {
                    message: HERDR_DOWN.into(),
                    fix: String::new(),
                })
            }),
            body,
            cursor: self.cursor.min(selectable.saturating_sub(1)),
            drawer_pr,
            help: self.help,
            help_scroll: self.help_scroll,
            filter: self.filter.clone(),
            status: self.status.clone(),
            herdr_ok: self.herdr_ok,
            herdr_down: self.herdr_down,
            agent_panes: (self.kind == ViewKind::Workspace && self.herdr_ok)
                .then(|| self.workspace_agent_panes().len()),
            now,
            clock: self.clock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    fn herdr_banner_only_when_no_github_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let mut a = app();
        // No snapshot yet is not "herdr down".
        assert_eq!(a.screen(now()).banner, None);
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &state,
            now(),
        );
        let s = a.screen(now());
        assert!(s.herdr_down);
        assert_eq!(
            s.banner.map(|b| b.message),
            Some("herdr not responding · agent state ?".into())
        );
        let bad = crate::store::Cache {
            error: Some(crate::github::GhError::LoginExpired),
            ..Default::default()
        };
        a.apply_fetch(FetchEvent::Done(bad), now());
        assert_eq!(
            a.screen(now()).banner.map(|b| b.message),
            Some("GitHub login expired".into())
        );
        with_agents(&mut a, &state);
        assert!(!a.screen(now()).herdr_down);
    }

    #[test]
    fn empty_mine_counts_the_other_scope() {
        let mut a = app();
        a.data.as_mut().unwrap().authored.clear();
        a.cycle_scope(); // authored
        let Body::Empty(EmptyInfo::Mine {
            scope,
            other_scope_count,
            repos,
        }) = a.screen(now()).body
        else {
            panic!("expected empty Mine")
        };
        assert_eq!(scope, Scope::Authored);
        assert_eq!(other_scope_count, 2);
        assert_eq!(repos, ["heard/docket", "heard/heard-ts"]);
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The Workspace copy in the audit workspace (own pane w1:p2), memory from the fixture.
    fn ws_app(dir: &Path) -> App {
        ws_app_in(dir, "w1:p2")
    }

    /// A Workspace copy whose own pane is `own`, memory from the fixture.
    fn ws_app_in(dir: &Path, own: &str) -> App {
        let state = &StateDir::new(dir.to_path_buf());
        let base = app();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.data = base.data;
        std::fs::write(
            dir.join(workspaces::FILE),
            include_str!("../tests/fixtures/workspaces.json"),
        )
        .unwrap();
        a.set_own_pane(own.into());
        a.set_path_exists(|_| true);
        let snap = crate::herdr::parse_snapshot(include_str!("../tests/fixtures/snapshot.json"));
        a.apply_snapshot(
            snap,
            &mut crate::git::FixtureBranches::embedded(),
            state,
            now(),
        );
        a
    }

    /// Moves the cursor down until it sits on PR `number`.
    fn select(a: &mut App, number: u32) {
        a.cursor = 0;
        for _ in 0..50 {
            if a.selected_pr().map(|p| p.number) == Some(number) {
                return;
            }
            a.handle_key(key(KeyCode::Char('j')));
        }
        panic!("#{number} not selectable");
    }

    #[test]
    fn keys_quit_move_and_refresh() {
        let mut a = app();
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Action::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), Action::Quit);
        assert_eq!(a.handle_key(key(KeyCode::Char('c'))), Action::None);
        assert_eq!(a.handle_key(key(KeyCode::Char('j'))), Action::None);
        assert_eq!(a.screen(now()).cursor, 1);
        a.handle_key(key(KeyCode::Down));
        a.handle_key(key(KeyCode::Up));
        a.handle_key(key(KeyCode::Char('k')));
        assert_eq!(a.screen(now()).cursor, 0);
        assert_eq!(a.handle_key(key(KeyCode::Char('r'))), Action::Refresh);
        a.handle_key(key(KeyCode::Char('s')));
        assert_eq!(a.screen(now()).header.scope, Scope::Authored);
        a.handle_key(key(KeyCode::Char('S')));
        assert_eq!(a.screen(now()).header.sort, SortOrder::Updated);
    }

    #[test]
    fn single_pr_workspace_opens_its_drawer() {
        let tmp = tempfile::tempdir().unwrap();
        // The notes workspace (w3) only has #1302.
        let a = ws_app_in(tmp.path(), "w3:p1");
        let s = a.screen(now());
        assert_eq!(s.drawer_pr.as_deref(), Some("PR_1302"));
        assert_eq!(a.selected_pr().map(|p| p.number), Some(1302));
    }

    #[test]
    fn multi_pr_workspace_keeps_the_drawer_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = ws_app(tmp.path());
        assert_eq!(a.screen(now()).drawer_pr, None);
    }

    #[test]
    fn closed_single_pr_drawer_stays_closed_across_refreshes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app_in(tmp.path(), "w3:p1");
        assert_eq!(a.screen(now()).drawer_pr.as_deref(), Some("PR_1302"));
        a.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(a.screen(now()).drawer_pr, None);
        refetch(&mut a, |_| true);
        assert_eq!(a.screen(now()).drawer_pr, None);
    }

    #[test]
    fn mine_view_never_opens_the_drawer_by_itself() {
        let mut a = app();
        refetch(&mut a, |p| p.number == 1279);
        assert_eq!(a.screen(now()).drawer_pr, None);
    }

    /// Feeds the app its own PRs again (those `keep` accepts), as a fresh fetch would.
    fn refetch(a: &mut App, keep: impl Fn(&Pr) -> bool) {
        let d = a.data.as_ref().unwrap();
        let prs: Vec<Pr> = d
            .groups
            .iter()
            .flat_map(|g| g.layers.iter().filter(|p| keep(p)).cloned())
            .collect();
        let authored: Vec<String> = d.authored.iter().cloned().collect();
        let assigned: Vec<String> = d.assigned.iter().cloned().collect();
        let (viewer, at) = (d.viewer.clone(), d.fetched_at);
        a.set_prs(prs, &authored, &assigned, &viewer, at);
    }

    #[test]
    fn scope_key_ignored_in_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app(tmp.path());
        a.handle_key(key(KeyCode::Char('s')));
        assert_eq!(a.screen(now()).header.scope, Scope::Both);
    }

    #[test]
    fn enter_focuses_stack_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = app();
        with_agents(&mut a, &StateDir::new(tmp.path().to_path_buf()));
        select(&mut a, 1291);
        assert_eq!(
            a.handle_key(key(KeyCode::Enter)),
            Action::Focus("w1:p1".into())
        );
        // The group header jumps too.
        a.cursor = 0;
        assert_eq!(
            a.handle_key(key(KeyCode::Enter)),
            Action::Focus("w1:p1".into())
        );
    }

    #[test]
    fn enter_without_agent_sets_status() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = app();
        with_agents(&mut a, &StateDir::new(tmp.path().to_path_buf()));
        select(&mut a, 1279);
        assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(
            a.screen(now()).status.as_deref(),
            Some("no agent pane for this stack")
        );
        // Any other key clears it.
        a.handle_key(key(KeyCode::Char('k')));
        assert_eq!(a.screen(now()).status, None);
    }

    #[test]
    fn enter_with_herdr_unreachable_sets_status() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let mut a = app();
        with_agents(&mut a, &state);
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &state,
            now(),
        );
        select(&mut a, 1291);
        assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);
        let s = a.screen(now());
        assert_eq!(s.status.as_deref(), Some("herdr unreachable"));
        assert!(!s.herdr_ok);
    }

    #[test]
    fn o_opens_selected_pr() {
        let mut a = app();
        select(&mut a, 412);
        assert_eq!(
            a.handle_key(key(KeyCode::Char('o'))),
            Action::Open("https://github.com/heard/docket/pull/412".into())
        );
    }

    #[test]
    fn workspace_shows_remembered_stacks_with_merged_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let a = ws_app(tmp.path());
        let s = a.screen(now());
        let Body::Groups(gs) = &s.body else { panic!() };
        let nums: Vec<Vec<u32>> = gs
            .iter()
            .map(|g| g.rows.iter().map(|r| r.pr.number).collect())
            .collect();
        assert_eq!(nums, [vec![1284, 1287, 1291, 1295], vec![1270]]);
        assert_eq!(s.header.branch.as_deref(), Some("audit/kafka-producer"));
        assert_eq!(s.header.prs, 4);
    }

    #[test]
    fn merged_row_has_no_attention() {
        let tmp = tempfile::tempdir().unwrap();
        let a = ws_app(tmp.path());
        let s = a.screen(now());
        let Body::Groups(gs) = &s.body else { panic!() };
        let row = gs
            .iter()
            .flat_map(|g| &g.rows)
            .find(|r| r.pr.number == 1270)
            .unwrap();
        assert_eq!(row.attention, None);
        assert_eq!(row.layer, 0);
        let c = &s.summary.counts;
        assert_eq!(
            c.urgent + c.needs_you + c.nudge + c.ready + c.waiting,
            4,
            "{c:?}"
        );
    }

    #[test]
    fn cursor_follows_pr_across_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().to_path_buf());
        let mut a = app();
        select(&mut a, 412);
        // Without agents the audit stack drops to NeedsYou; it was first, so thread-model
        // (updated later) may move. The cursor must stay on #412 either way.
        let mut cache = good_cache();
        for p in &mut cache.prs {
            if p.number == 1279 {
                p.changes_requested = true; // standalone #1279 now ranks above thread-model
                p.updated_at = "2026-10-17T14:31:00Z".parse().unwrap();
            }
        }
        a.apply_fetch(FetchEvent::Done(cache), now());
        assert_eq!(a.selected_pr().map(|p| p.number), Some(412));
        with_agents(&mut a, &state);
        assert_eq!(a.selected_pr().map(|p| p.number), Some(412));

        // A group header stays on its group.
        let key = audit_key(&a);
        a.cursor = 0;
        while a.selected_group().map(|g| g.key) != Some(key.clone()) {
            a.handle_key(self::key(KeyCode::Char('j')));
        }
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &state,
            now(),
        );
        assert_eq!(a.selected_group().map(|g| g.key), Some(key));

        // A PR that disappears leaves the cursor clamped.
        select(&mut a, 1302);
        let mut cache = good_cache();
        cache.prs.retain(|p| p.number != 1302);
        a.apply_fetch(FetchEvent::Done(cache), now());
        let s = a.screen(now());
        let Body::Groups(gs) = &s.body else { panic!() };
        let selectable: usize = gs.iter().map(|g| 1 + g.rows.len()).sum();
        assert_eq!(s.cursor, selectable - 1);
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

    fn type_text(a: &mut App, text: &str) {
        for c in text.chars() {
            a.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn names(a: &App) -> Vec<String> {
        groups(a).into_iter().map(|g| g.name).collect()
    }

    #[test]
    fn filter_by_number() {
        for q in ["#412", "412"] {
            let mut a = app();
            a.handle_key(key(KeyCode::Char('/')));
            type_text(&mut a, q);
            assert_eq!(names(&a), ["thread-model"], "{q}");
            // Matching groups show whole.
            assert_eq!(groups(&a)[0].rows.len(), 2);
        }
    }

    #[test]
    fn filter_by_repo_and_title_ignoring_case() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        type_text(&mut a, "DOCKET");
        assert!(groups(&a).iter().all(|g| g.repo == "heard/docket"));
        assert!(!groups(&a).is_empty());
        a.handle_key(key(KeyCode::Esc));
        a.handle_key(key(KeyCode::Char('/')));
        type_text(&mut a, "Tokenizer");
        assert_eq!(groups(&a).len(), 1);
    }

    #[test]
    fn filter_editing_keys_apply_and_clear() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        // q is text while editing; Backspace deletes.
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Action::None);
        type_text(&mut a, "x");
        a.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            a.screen(now()).filter,
            Some(FilterState {
                text: "q".into(),
                editing: true
            })
        );
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), Action::Quit);
        a.handle_key(key(KeyCode::Enter));
        assert!(!a.screen(now()).filter.unwrap().editing);
        // Applied: normal keys work again, Esc clears.
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Action::Quit);
        a.handle_key(key(KeyCode::Esc));
        assert_eq!(a.screen(now()).filter, None);
    }

    #[test]
    fn filter_does_not_change_summary_counts() {
        let mut a = app();
        let before = a.screen(now());
        a.handle_key(key(KeyCode::Char('/')));
        type_text(&mut a, "#412");
        let after = a.screen(now());
        assert_eq!(after.summary.counts, before.summary.counts);
        assert_eq!(after.header.stacks, 1);
        assert!(after.header.prs < before.header.prs);
    }

    #[test]
    fn sort_cycles() {
        let mut a = app();
        let urgency = names(&a);
        a.handle_key(key(KeyCode::Char('S')));
        assert_eq!(a.screen(now()).header.sort, SortOrder::Updated);
        let updated_at = |g: &GroupView| g.rows.iter().map(|r| r.pr.updated_at).max().unwrap();
        let gs = groups(&a);
        assert!(
            gs.windows(2)
                .all(|w| updated_at(&w[0]) >= updated_at(&w[1]))
        );
        a.handle_key(key(KeyCode::Char('S')));
        assert_eq!(a.screen(now()).header.sort, SortOrder::Repo);
        let gs = groups(&a);
        assert!(gs.windows(2).all(|w| w[0].repo <= w[1].repo));
        assert!(
            gs.windows(2)
                .all(|w| w[0].repo != w[1].repo || w[0].rank >= w[1].rank)
        );
        a.handle_key(key(KeyCode::Char('S')));
        assert_eq!(a.screen(now()).header.sort, SortOrder::Urgency);
        assert_eq!(names(&a), urgency);
    }

    #[test]
    fn fold_and_unfold_keep_cursor_on_header() {
        let mut a = app();
        select(&mut a, 412);
        a.handle_key(key(KeyCode::Char('h')));
        let g = a.selected_group().unwrap();
        assert!(g.folded);
        assert_eq!(a.selection(), Some(Selection::Group(g.key.clone())));
        a.handle_key(key(KeyCode::Char('l')));
        assert!(!a.selected_group().unwrap().folded);
        a.handle_key(key(KeyCode::Left));
        assert!(a.selected_group().unwrap().folded);
        a.handle_key(key(KeyCode::Right));
        assert!(!a.selected_group().unwrap().folded);
    }

    #[test]
    fn fold_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let state = || StateDir::new(tmp.path().to_path_buf());
        let mut a = app().with_state(state());
        select(&mut a, 412);
        a.handle_key(key(KeyCode::Char('h')));
        let key412 = a.selected_group().unwrap().key;
        let saved: BTreeSet<String> = state().load("folds.json").unwrap();
        assert_eq!(saved, BTreeSet::from([key412.clone()]));

        let b = app().with_state(state());
        let g = groups(&b).into_iter().find(|g| g.key == key412).unwrap();
        assert!(g.folded);

        a.handle_key(key(KeyCode::Char('l')));
        let saved: BTreeSet<String> = state().load("folds.json").unwrap();
        assert!(saved.is_empty());
    }

    #[test]
    fn help_toggles_scrolls_and_swallows_keys() {
        let mut a = app();
        assert!(!a.screen(now()).help);
        a.handle_key(key(KeyCode::Char('?')));
        assert!(a.screen(now()).help);
        let c = a.screen(now()).cursor;
        a.handle_key(key(KeyCode::Char('j')));
        a.handle_key(key(KeyCode::Char('j')));
        assert_eq!(a.screen(now()).help_scroll, 2);
        assert_eq!(a.screen(now()).cursor, c, "j scrolls help, not the cursor");
        a.handle_key(key(KeyCode::Char('k')));
        assert_eq!(a.screen(now()).help_scroll, 1);
        assert_eq!(a.handle_key(key(KeyCode::Char('r'))), Action::None);
        assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);
        assert!(a.screen(now()).help);
        a.handle_key(key(KeyCode::Esc));
        assert!(!a.screen(now()).help);
        a.handle_key(key(KeyCode::Char('?')));
        assert_eq!(a.screen(now()).help_scroll, 0);
        a.handle_key(key(KeyCode::Char('?')));
        assert!(!a.screen(now()).help);
        a.handle_key(key(KeyCode::Char('?')));
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn question_mark_is_text_while_filter_editing() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('/')));
        a.handle_key(key(KeyCode::Char('?')));
        assert!(!a.screen(now()).help);
    }

    // ------------------------------------------------------------ visits and ✦ marks (§9)

    fn at(s: &str) -> DateTime<Utc> {
        format!("2026-10-17T{s}Z").parse().unwrap()
    }

    /// Board 6 (`seed_board6`), plus the fixture workspace memory.
    fn changed_state(dir: &Path) -> StateDir {
        let state = StateDir::new(dir.to_path_buf());
        crate::seed_board6(&state).unwrap();
        std::fs::write(
            dir.join(workspaces::FILE),
            include_str!("../tests/fixtures/workspaces.json"),
        )
        .unwrap();
        state
    }

    /// A copy on `dir`'s state, its own pane `pane`, after a snapshot with `focused` the
    /// focused workspace (so the pane is visible when it is in that workspace).
    fn copy(kind: ViewKind, dir: &Path, pane: &str, focused: &str, t: &str) -> App {
        let mut a = App::new(kind, FixedOffset::east_opt(0).unwrap())
            .with_state(StateDir::new(dir.to_path_buf()));
        a.data = app().data;
        a.set_own_pane(pane.into());
        a.set_path_exists(|_| true);
        look(&mut a, dir, focused, t);
        a
    }

    /// A herdr snapshot with workspace `focused` in front, applied at `t`.
    fn look(a: &mut App, dir: &Path, focused: &str, t: &str) {
        let mut snap =
            crate::herdr::parse_snapshot(include_str!("../tests/fixtures/snapshot.json")).unwrap();
        snap.focused_workspace_id = Some(focused.into());
        a.apply_snapshot(
            Ok(snap),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(dir.to_path_buf()),
            at(t),
        );
    }

    fn marked(a: &App, t: &str) -> Vec<u32> {
        let Body::Groups(gs) = a.screen(at(t)).body else {
            panic!("expected groups")
        };
        gs.iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.changed)
            .map(|r| r.pr.number)
            .collect()
    }

    #[test]
    fn visit_marks_persist_for_whole_visit() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        // Mine lives in w4; focusing w1 hides it.
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w1", "14:32:20");
        assert!(a.visit.is_none());
        look(&mut a, tmp.path(), "w4", "14:32:26");
        assert_eq!(marked(&a, "14:32:26"), [1284, 1287, 1291, 412]);
        // Stamped on disk, yet the marks stay for the whole visit.
        look(&mut a, tmp.path(), "w4", "14:40:00");
        assert_eq!(marked(&a, "14:40:00"), [1284, 1287, 1291, 412]);
        assert_eq!(marked(&a, "14:50:00"), [1284, 1287, 1291, 412]);
        let s = a.screen(at("14:50:00"));
        assert_eq!(s.summary.changed, 4);
        assert_eq!(s.summary.since, Some(at("14:18:00")));
        let Body::Groups(gs) = s.body else { panic!() };
        let r1287 = gs[0].rows.iter().find(|r| r.pr.number == 1287).unwrap();
        assert_eq!(
            r1287.change_lines,
            [
                "✗ CI failed: test-integration 17m ago",
                "✗ @art requested changes 17m ago",
                "◇ @claude[bot] commented 17m ago",
                "◇ 3 new thread(s) 17m ago",
            ]
        );
        // Leave and come back: nothing new since.
        look(&mut a, tmp.path(), "w1", "14:51:00");
        assert!(a.visit.is_none());
        look(&mut a, tmp.path(), "w4", "14:52:00");
        assert!(marked(&a, "14:52:00").is_empty());
        let s = a.screen(at("14:52:00"));
        assert_eq!((s.summary.changed, s.summary.since), (0, None));
    }

    fn unreachable(a: &mut App, dir: &Path, t: &str) {
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(dir.to_path_buf()),
            at(t),
        );
    }

    /// One failed snapshot is a blip: the visit (and its marks) survives it. Two in a row
    /// end it; a snapshot that says hidden ends it at once.
    #[test]
    fn herdr_unreachable_twice_ends_the_visit() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:32:26");
        assert!(a.visit.is_some());
        unreachable(&mut a, tmp.path(), "14:33:00");
        assert!(a.visit.is_some());
        assert_eq!(marked(&a, "14:33:00"), [1284, 1287, 1291, 412]);
        // Reachable again: the count starts over.
        look(&mut a, tmp.path(), "w4", "14:33:02");
        unreachable(&mut a, tmp.path(), "14:33:04");
        assert!(a.visit.is_some());
        unreachable(&mut a, tmp.path(), "14:33:06");
        assert!(a.visit.is_none());
        // A failed snapshot never starts a visit.
        unreachable(&mut a, tmp.path(), "14:33:08");
        assert!(a.visit.is_none());
        look(&mut a, tmp.path(), "w4", "14:33:10");
        look(&mut a, tmp.path(), "w1", "14:33:12");
        assert!(a.visit.is_none());
    }

    #[test]
    fn other_workspace_visit_does_not_clear() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        // w1's copy is hidden (w2 in front) while w2's copy is visited.
        let mut w1 = copy(ViewKind::Workspace, tmp.path(), "w1:p2", "w2", "14:32:20");
        let w2 = copy(ViewKind::Workspace, tmp.path(), "w2:p1", "w2", "14:32:20");
        assert_eq!(marked(&w2, "14:32:20"), [412]);
        look(&mut w1, tmp.path(), "w1", "14:40:00");
        assert_eq!(marked(&w1, "14:40:00"), [1284, 1287, 1291]);
        // The docket copy's next visit finds nothing new.
        let w2 = copy(ViewKind::Workspace, tmp.path(), "w2:p1", "w2", "14:41:00");
        assert!(marked(&w2, "14:41:00").is_empty());
    }

    #[test]
    fn mine_visit_does_not_clear_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        let mine = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:32:20");
        assert_eq!(marked(&mine, "14:32:20"), [1284, 1287, 1291, 412]);
        let ws = copy(ViewKind::Workspace, tmp.path(), "w1:p2", "w1", "14:40:00");
        assert_eq!(marked(&ws, "14:40:00"), [1284, 1287, 1291]);
        // And the Workspace visit clears them in Mine; #412 was only seen in Mine.
        let mine = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:41:00");
        assert!(marked(&mine, "14:41:00").is_empty());
    }

    #[test]
    fn workspace_visit_clears_mine_but_not_other_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        copy(ViewKind::Workspace, tmp.path(), "w1:p2", "w1", "14:40:00");
        let mine = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:41:00");
        assert_eq!(marked(&mine, "14:41:00"), [412]);
    }

    #[test]
    fn changes_during_a_visit_clear_on_the_next() {
        let tmp = tempfile::tempdir().unwrap();
        let state = changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:32:20");
        // #1279 changes during the visit: marked now, and on the next visit no more.
        let mut prs = model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap();
        let p = prs.iter_mut().find(|p| p.number == 1279).unwrap();
        p.head_oid = "new".into();
        state
            .update::<crate::changes::Changes>(crate::changes::CHANGES_FILE, |c| {
                c.observe(&prs, at("14:35:00"))
            })
            .unwrap();
        let mut cache = good_cache();
        cache.prs = prs;
        cache.fetched_at = Some(at("14:35:00"));
        a.apply_fetch(FetchEvent::Done(cache), at("14:35:00"));
        assert_eq!(marked(&a, "14:36:00"), [1284, 1287, 1291, 412, 1279]);
        look(&mut a, tmp.path(), "w1", "14:37:00");
        look(&mut a, tmp.path(), "w4", "14:38:00");
        assert!(marked(&a, "14:38:00").is_empty());
    }

    /// A change in `changes.json` with this PR, recorded at `t` (as another copy's
    /// worker would, without this copy seeing a `Done`).
    fn record_change(state: &StateDir, number: u32, t: &str) {
        let mut prs = model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap();
        let p = prs.iter_mut().find(|p| p.number == number).unwrap();
        p.head_oid = format!("pushed-{t}");
        state
            .update::<crate::changes::Changes>(crate::changes::CHANGES_FILE, |c| {
                c.observe(&prs, at(t))
            })
            .unwrap();
    }

    #[test]
    fn visit_start_reads_changes_another_copy_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let state = changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w1", "14:32:20");
        record_change(&state, 1279, "14:35:00");
        look(&mut a, tmp.path(), "w4", "14:36:00");
        assert!(marked(&a, "14:36:00").contains(&1279));
    }

    #[test]
    fn change_saved_after_the_visit_stamp_is_marked_next_visit() {
        let tmp = tempfile::tempdir().unwrap();
        let state = changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w4", "14:33:00");
        // Another copy's fetch (started 14:32:50) saves its change at 14:34; this copy
        // leaves before any Done of its own.
        record_change(&state, 1279, "14:34:00");
        look(&mut a, tmp.path(), "w1", "14:35:00");
        look(&mut a, tmp.path(), "w4", "14:36:00");
        assert_eq!(marked(&a, "14:36:00"), [1279]);
    }

    #[test]
    fn failed_seen_write_keeps_the_baselines() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        let state = StateDir::new(tmp.path().to_path_buf());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w1", "14:32:20");
        // After this copy loaded seen.json, the audit workspace's copy saw its PRs.
        let audit: Vec<String> = ["PR_1284", "PR_1287", "PR_1291"].map(String::from).into();
        state
            .update::<crate::changes::Seen>(crate::changes::SEEN_FILE, |s| {
                crate::changes::stamp(s, &audit, ViewKind::Workspace, at("14:32:24"))
            })
            .unwrap();
        // seen.json can be read and locked but not replaced.
        std::fs::create_dir(tmp.path().join("seen.json.tmp")).unwrap();
        look(&mut a, tmp.path(), "w4", "14:32:26");
        assert!(
            a.status
                .as_deref()
                .is_some_and(|s| s.starts_with("seen not saved"))
        );
        // Baselines from the file as read, not from the stale copy in memory.
        assert_eq!(marked(&a, "14:32:26"), [412]);
    }

    /// Only rows actually listed are stamped seen: a folded group's PRs and those the
    /// filter hides keep their marks for the next visit.
    #[test]
    fn folded_and_filtered_prs_are_not_stamped() {
        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w1", "14:32:20");
        let docket = a.data.as_ref().unwrap().groups.iter();
        let docket = docket
            .filter(|g| g.repo == "heard/docket")
            .map(|g| g.key.clone());
        a.folds.extend(docket);
        look(&mut a, tmp.path(), "w4", "14:32:26");
        look(&mut a, tmp.path(), "w1", "14:33:00");
        a.folds.clear();
        look(&mut a, tmp.path(), "w4", "14:34:00");
        assert_eq!(marked(&a, "14:34:00"), [412]);

        let tmp = tempfile::tempdir().unwrap();
        changed_state(tmp.path());
        let mut a = copy(ViewKind::Mine, tmp.path(), "w4:p1", "w1", "14:32:20");
        a.filter = Some(FilterState {
            text: "audit".into(),
            editing: false,
        });
        look(&mut a, tmp.path(), "w4", "14:32:26");
        look(&mut a, tmp.path(), "w1", "14:33:00");
        a.filter = None;
        look(&mut a, tmp.path(), "w4", "14:34:00");
        assert_eq!(marked(&a, "14:34:00"), [412]);
    }

    #[test]
    fn without_state_nothing_is_marked() {
        let mut a = app();
        a.set_visible(true, now());
        assert!(marked(&a, "14:32:26").is_empty());
        assert_eq!(a.screen(now()).summary.changed, 0);
    }
}
