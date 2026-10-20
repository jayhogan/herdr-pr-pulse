//! View model for the Review view (review design §5, §7): which PRs are listed, how they
//! group and order, and what each key does.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, Banner, LoadProgress, SPINNER};
use crate::config::ReviewConfig;
use crate::review::fetch::{ReviewCache, ReviewEvent};
use crate::review::model::ReviewPr;
use crate::store::StateDir;

/// `review-folds.json`: a `BTreeSet<String>` of folded group keys.
const FOLDS_FILE: &str = "review-folds.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    User,
    Repo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    NotApproved,
    Approved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRow {
    pub pr: ReviewPr,
    /// One of the viewer's teams is a requested code owner (★).
    pub code_owner: bool,
    /// The viewer requested changes and the author hasn't pushed since (↩, dim).
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewGroup {
    /// `user:<login>` or `repo:<owner/name>`.
    pub key: String,
    /// The login, or the repo without its owner.
    pub name: String,
    /// The earliest ready time among its rows.
    pub oldest: DateTime<Utc>,
    pub folded: bool,
    pub rows: Vec<ReviewRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReviewBody {
    Loading(LoadProgress),
    NotConfigured {
        path: String,
    },
    /// Nothing in this filter; `other` PRs are in the other one.
    Empty {
        repos: usize,
        show: Show,
        other: usize,
    },
    Groups(Vec<ReviewGroup>),
}

#[derive(Debug, PartialEq)]
pub struct ReviewScreen {
    /// Over the listed PRs; `repos` is the configured count when nothing is listed.
    pub prs: usize,
    pub people: usize,
    pub repos: usize,
    pub group_by: GroupBy,
    pub show: Show,
    pub code_owner: usize,
    pub waiting: usize,
    pub viewer: String,
    pub fetched_at: Option<DateTime<Utc>>,
    pub fetching: bool,
    /// The last fetch failed; rows (if any) are from an earlier one.
    pub stale: bool,
    /// The spinner's current frame.
    pub spinner: &'static str,
    pub banner: Option<Banner>,
    /// The team lookup failed, so no row can show ★.
    pub teams_missing: bool,
    pub body: ReviewBody,
    /// Index into selectable lines: group headers and the rows of unfolded groups.
    pub cursor: usize,
    /// The PR whose details are open (when it is listed).
    pub drawer_pr: Option<String>,
    pub help: bool,
    /// First visible line of the help overlay.
    pub help_scroll: usize,
    pub status: Option<String>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    Group(String),
    Pr(String),
}

pub struct ReviewApp {
    config: ReviewConfig,
    /// Where the config file is, for fixes that point at it.
    config_path: Option<PathBuf>,
    group_by: GroupBy,
    show: Show,
    cursor: usize,
    /// The line last chosen with a key: the cursor goes back to it whenever it is listed
    /// again (after `a` twice, say), and to the nearest line while it is not.
    anchor: Option<Selection>,
    /// Id of the PR the details are open under.
    drawer: Option<String>,
    help: bool,
    help_scroll: usize,
    /// How far the overlay can scroll, as last drawn.
    help_max: usize,
    folds: BTreeSet<String>,
    /// Where folds persist; without one they live in memory only.
    state: Option<StateDir>,
    /// One-line message shown in place of the key hints until the next key.
    status: Option<String>,
    spin: usize,
    /// The last fetch outcome; data once `fetched_at` is set.
    cache: Option<ReviewCache>,
    fetching: bool,
    progress: LoadProgress,
}

/// The fixture data as a running app would show it (tests and `--fixtures`).
pub fn fixture_app() -> ReviewApp {
    let repos = super::FIXTURE_REPOS.map(String::from).to_vec();
    let mut app = ReviewApp::new(ReviewConfig::Repos(repos));
    app.set_cache(super::fixture_cache().expect("fixture files parse"));
    app
}

fn short_repo(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

impl ReviewApp {
    pub fn new(config: ReviewConfig) -> ReviewApp {
        ReviewApp {
            config,
            config_path: None,
            group_by: GroupBy::User,
            show: Show::NotApproved,
            cursor: 0,
            anchor: None,
            drawer: None,
            help: false,
            help_scroll: 0,
            help_max: usize::MAX,
            folds: BTreeSet::new(),
            state: None,
            status: None,
            spin: 0,
            cache: None,
            fetching: false,
            progress: LoadProgress::default(),
        }
    }

    /// Loads the persisted folds from `state` and saves later ones there.
    pub fn with_state(mut self, state: StateDir) -> ReviewApp {
        match state.load::<BTreeSet<String>>(FOLDS_FILE) {
            Ok(folds) => self.folds = folds,
            Err(e) => self.status = Some(format!("folds not loaded: {e}")),
        }
        self.state = Some(state);
        self
    }

    /// The config file's location, named in the fix for a repo GitHub can't search.
    pub fn with_config_path(mut self, path: PathBuf) -> ReviewApp {
        self.config_path = Some(path);
        self
    }

    /// The configured repos; empty when the config has none or is broken.
    pub fn repos(&self) -> Vec<String> {
        match &self.config {
            ReviewConfig::Repos(repos) => repos.clone(),
            _ => Vec::new(),
        }
    }

    pub fn apply(&mut self, ev: ReviewEvent) {
        match ev {
            ReviewEvent::Started => self.fetching = true,
            ReviewEvent::Progress(repos) => self.progress = LoadProgress { repos },
            ReviewEvent::Done(cache) => {
                self.fetching = false;
                self.set_cache(cache);
            }
        }
    }

    /// Shows `cache`, keeping the cursor on the same line when it is still listed.
    pub fn set_cache(&mut self, cache: ReviewCache) {
        self.cache = Some(cache);
        self.restore();
    }

    pub fn advance_spinner(&mut self) {
        self.spin = (self.spin + 1) % SPINNER.len();
    }

    pub fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    /// The renderer reports how far the help overlay can scroll; keep the offset within it.
    pub fn set_help_max(&mut self, max: usize) {
        self.help_max = max;
        self.help_scroll = self.help_scroll.min(max);
    }

    /// Applies a key press. Every key clears the status line first.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.status = None;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        if self.help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.help = false,
                KeyCode::Char('q') => return Action::Quit,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll = (self.help_scroll + 1).min(self.help_max);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                _ => {}
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char(' ') => match self.selection() {
                Some(Selection::Group(key)) => self.set_fold(&key, !self.folds.contains(&key)),
                Some(Selection::Pr(id)) => {
                    self.drawer = (self.drawer.as_ref() != Some(&id)).then_some(id);
                }
                None => {}
            },
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(key) = self.selected_group_key() {
                    self.set_fold(&key, true);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(key) = self.selected_group_key() {
                    self.set_fold(&key, false);
                }
            }
            KeyCode::Char('o') => {
                if let Some(p) = self.selected_pr() {
                    return Action::Open(p.pr.url);
                }
            }
            KeyCode::Char('r') => return Action::Refresh,
            KeyCode::Char('g') => {
                self.group_by = match self.group_by {
                    GroupBy::User => GroupBy::Repo,
                    GroupBy::Repo => GroupBy::User,
                };
                self.restore();
            }
            KeyCode::Char('a') => {
                self.show = self.other_show();
                self.restore();
            }
            _ => {}
        }
        Action::None
    }

    /// The PR under the cursor, if the cursor is on a PR row.
    pub fn selected_pr(&self) -> Option<ReviewPr> {
        let Some(Selection::Pr(id)) = self.selection() else {
            return None;
        };
        self.groups()
            .into_iter()
            .flat_map(|g| g.rows)
            .find(|r| r.pr.pr.id == id)
            .map(|r| r.pr)
    }

    pub fn screen(&self, now: DateTime<Utc>) -> ReviewScreen {
        let configured = self.repos().len();
        let data = self.cache.as_ref().filter(|c| c.fetched_at.is_some());
        let error = self.cache.as_ref().and_then(|c| c.error.as_ref());
        let banner = match (&self.config, error) {
            (ReviewConfig::Invalid { path, message }, _) => Some(Banner {
                message: format!("config: {message}"),
                fix: format!("Fix {}, then relaunch the pane.", path.display()),
            }),
            (_, Some(e)) => {
                let message = e.message();
                // GitHub rejects the whole search when a configured repo is missing or
                // hidden: retrying can't help, the config can.
                let unsearchable = message.contains("cannot be searched")
                    || message.contains("Could not resolve to a Repository");
                let fix = match (&self.config_path, unsearchable) {
                    (Some(path), true) => format!(
                        "Check the repos in {}, then relaunch the pane.",
                        path.display()
                    ),
                    _ => e.fix(),
                };
                Some(Banner { message, fix })
            }
            _ => None,
        };
        let groups = self.groups();
        let rows = || groups.iter().flat_map(|g| &g.rows);
        let distinct = |f: fn(&ReviewRow) -> &str| rows().map(f).collect::<BTreeSet<_>>().len();
        let body = match (&self.config, data) {
            (ReviewConfig::Missing { path } | ReviewConfig::Invalid { path, .. }, _) => {
                ReviewBody::NotConfigured {
                    path: path.display().to_string(),
                }
            }
            (_, None) => ReviewBody::Loading(self.progress.clone()),
            (_, Some(c)) if groups.is_empty() => ReviewBody::Empty {
                repos: configured,
                show: self.show,
                other: self.listed(c, self.other_show()).count(),
            },
            _ => ReviewBody::Groups(groups.clone()),
        };
        let repos = match body {
            ReviewBody::Groups(_) => distinct(|r| &r.pr.pr.repo),
            _ => configured,
        };
        let drawer_pr = self
            .drawer
            .clone()
            .filter(|id| rows().any(|r| &r.pr.pr.id == id));
        ReviewScreen {
            prs: rows().count(),
            people: distinct(|r| &r.pr.pr.author),
            repos,
            group_by: self.group_by,
            show: self.show,
            code_owner: rows().filter(|r| r.code_owner).count(),
            waiting: rows().filter(|r| r.waiting).count(),
            viewer: data.map(|c| c.viewer.clone()).unwrap_or_default(),
            fetched_at: data.and_then(|c| c.fetched_at),
            fetching: self.fetching,
            stale: data.is_some() && error.is_some(),
            spinner: SPINNER[self.spin],
            banner,
            teams_missing: data.is_some_and(|c| c.teams.is_none()),
            body,
            cursor: self.cursor,
            drawer_pr,
            help: self.help,
            help_scroll: self.help_scroll,
            status: self.status.clone(),
            now,
        }
    }

    fn other_show(&self) -> Show {
        match self.show {
            Show::NotApproved => Show::Approved,
            Show::Approved => Show::NotApproved,
        }
    }

    /// The PRs `show` lists from `c`.
    fn listed<'a>(&self, c: &'a ReviewCache, show: Show) -> impl Iterator<Item = &'a ReviewPr> {
        let viewer = c.viewer.clone();
        c.prs
            .iter()
            .filter(move |p| p.included(&viewer) && p.approved() == (show == Show::Approved))
    }

    /// The listed PRs, grouped: groups by their oldest ready time, rows by ready time, then
    /// repo and number.
    fn groups(&self) -> Vec<ReviewGroup> {
        let Some(c) = self.cache.as_ref().filter(|c| c.fetched_at.is_some()) else {
            return Vec::new();
        };
        let mut groups: Vec<ReviewGroup> = Vec::new();
        for p in self.listed(c, self.show) {
            let (key, name) = match self.group_by {
                GroupBy::User => (format!("user:{}", p.pr.author), p.pr.author.clone()),
                GroupBy::Repo => (
                    format!("repo:{}", p.pr.repo),
                    short_repo(&p.pr.repo).to_string(),
                ),
            };
            let teams = c
                .teams
                .as_ref()
                .and_then(|t| t.get(p.org()))
                .map_or(&[][..], Vec::as_slice);
            let row = ReviewRow {
                code_owner: p.code_owner(&c.viewer, teams),
                waiting: p.waiting_on_author(&c.viewer),
                pr: p.clone(),
            };
            match groups.iter_mut().find(|g| g.key == key) {
                Some(g) => g.rows.push(row),
                None => groups.push(ReviewGroup {
                    folded: self.folds.contains(&key),
                    key,
                    name,
                    oldest: p.ready_at,
                    rows: vec![row],
                }),
            }
        }
        for g in &mut groups {
            g.rows.sort_by(|a, b| {
                let k = |r: &ReviewRow| (r.pr.ready_at, r.pr.pr.repo.clone(), r.pr.pr.number);
                k(a).cmp(&k(b))
            });
            g.oldest = g.rows[0].pr.ready_at;
        }
        groups.sort_by(|a, b| (a.oldest, &a.name).cmp(&(b.oldest, &b.name)));
        groups
    }

    /// Every selectable line, in screen order.
    fn selectables(&self) -> Vec<Selection> {
        let mut out = Vec::new();
        for g in self.groups() {
            out.push(Selection::Group(g.key.clone()));
            if !g.folded {
                out.extend(g.rows.iter().map(|r| Selection::Pr(r.pr.pr.id.clone())));
            }
        }
        out
    }

    fn selection(&self) -> Option<Selection> {
        self.selectables().into_iter().nth(self.cursor)
    }

    /// The group of the line under the cursor.
    fn selected_group_key(&self) -> Option<String> {
        let sel = self.selectables();
        sel[..sel.len().min(self.cursor + 1)]
            .iter()
            .rev()
            .find_map(|s| match s {
                Selection::Group(key) => Some(key.clone()),
                Selection::Pr(_) => None,
            })
    }

    /// After the list changed: the cursor goes to the anchor if it is listed, else it stays
    /// within the list.
    fn restore(&mut self) {
        let all = self.selectables();
        let found = self
            .anchor
            .as_ref()
            .and_then(|a| all.iter().position(|x| x == a));
        self.cursor = found.unwrap_or(self.cursor.min(all.len().saturating_sub(1)));
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.selectables().len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.anchor = self.selection();
    }

    /// Folds or unfolds group `key`, persisting the change, and puts the cursor on its header.
    fn set_fold(&mut self, key: &str, fold: bool) {
        if self.folds.contains(key) != fold {
            let apply = |folds: &mut BTreeSet<String>| {
                if fold {
                    folds.insert(key.to_string())
                } else {
                    folds.remove(key)
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
        }
        self.anchor = Some(Selection::Group(key.to_string()));
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::RepoStatus;
    use crate::github::GhError;

    fn now() -> DateTime<Utc> {
        "2026-10-20T15:00:00Z".parse().unwrap()
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn app() -> ReviewApp {
        fixture_app()
    }

    fn groups(a: &ReviewApp) -> Vec<ReviewGroup> {
        match a.screen(now()).body {
            ReviewBody::Groups(gs) => gs,
            other => panic!("expected groups, got {other:?}"),
        }
    }

    fn numbers(a: &ReviewApp) -> Vec<Vec<u32>> {
        groups(a)
            .iter()
            .map(|g| g.rows.iter().map(|r| r.pr.pr.number).collect())
            .collect()
    }

    fn id_of(n: u32) -> String {
        format!("PR_{n}")
    }

    /// Moves the cursor down until it is on PR `n`.
    fn select(a: &mut ReviewApp, n: u32) {
        for _ in 0..40 {
            if a.selected_pr().is_some_and(|p| p.pr.number == n) {
                return;
            }
            a.handle_key(key('j'));
        }
        panic!("#{n} not reachable");
    }

    fn cache_with(keep: impl Fn(u32) -> bool) -> ReviewCache {
        let mut c = super::super::fixture_cache().unwrap();
        c.prs.retain(|p| keep(p.pr.number));
        c
    }

    #[test]
    fn groups_by_user_in_age_order() {
        let a = app();
        let names: Vec<String> = groups(&a).into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["maria-chen", "dan-o", "priya-s", "ghost", "sam-k"]);
        assert_eq!(
            numbers(&a),
            [
                vec![1298, 1312, 1320],
                vec![87, 95, 91],
                vec![430],
                vec![1333],
                vec![418]
            ]
        );
        let s = a.screen(now());
        assert_eq!((s.prs, s.people, s.repos), (9, 5, 3));
        assert_eq!((s.code_owner, s.waiting), (2, 1));
        assert_eq!(s.viewer, "jayhogan");
        assert_eq!(groups(&a)[0].key, "user:maria-chen");
        assert_eq!(
            groups(&a)[0].oldest,
            "2026-10-16T15:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        let rows = &groups(&a)[1].rows;
        assert!(rows[2].waiting && !rows[0].waiting);
        assert!(groups(&a)[0].rows[0].code_owner);
    }

    #[test]
    fn g_groups_by_repo() {
        let mut a = app();
        a.handle_key(key('g'));
        assert_eq!(a.screen(now()).group_by, GroupBy::Repo);
        assert_eq!(
            numbers(&a),
            [
                vec![1298, 1312, 1333, 1320],
                vec![87, 95, 91],
                vec![430, 418]
            ]
        );
        let gs = groups(&a);
        assert_eq!(gs[0].name, "heard-ts");
        assert_eq!(gs[0].key, "repo:heard/heard-ts");
        a.handle_key(key('g'));
        assert_eq!(a.screen(now()).group_by, GroupBy::User);
    }

    #[test]
    fn a_shows_approved() {
        let mut a = app();
        a.handle_key(key('a'));
        let s = a.screen(now());
        assert_eq!(s.show, Show::Approved);
        let mut listed = numbers(&a).concat();
        listed.sort();
        assert_eq!(listed, [421, 431]);
    }

    #[test]
    fn space_folds_header_and_opens_drawer_on_row() {
        let mut a = app();
        a.handle_key(key(' ')); // cursor 0: maria-chen's header
        assert!(groups(&a)[0].folded);
        assert_eq!(a.screen(now()).cursor, 0);
        a.handle_key(key(' '));
        assert!(!groups(&a)[0].folded);
        a.handle_key(key('j'));
        a.handle_key(key(' '));
        assert_eq!(a.screen(now()).drawer_pr, Some(id_of(1298)));
        a.handle_key(key(' '));
        assert_eq!(a.screen(now()).drawer_pr, None);
    }

    #[test]
    fn h_folds_from_a_row_onto_its_header_and_l_unfolds() {
        let mut a = app();
        select(&mut a, 95);
        a.handle_key(key('h'));
        assert!(groups(&a)[1].folded);
        assert_eq!(a.screen(now()).cursor, 4); // maria header + 3 rows, then dan-o's header
        a.handle_key(key('l'));
        assert!(!groups(&a)[1].folded);
    }

    #[test]
    fn o_opens_row_url_and_nothing_on_header() {
        let mut a = app();
        assert_eq!(a.handle_key(key('o')), Action::None);
        a.handle_key(key('j'));
        assert_eq!(
            a.handle_key(key('o')),
            Action::Open("https://github.com/heard/heard-ts/pull/1298".into())
        );
    }

    #[test]
    fn keys_refresh_quit_and_help() {
        let mut a = app();
        assert_eq!(a.handle_key(key('r')), Action::Refresh);
        a.handle_key(key('?'));
        assert!(a.screen(now()).help);
        a.handle_key(key('j')); // scrolls help, not the list
        assert_eq!(a.screen(now()).cursor, 0);
        a.handle_key(key('?'));
        assert!(!a.screen(now()).help);
        assert_eq!(a.handle_key(key('q')), Action::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), Action::Quit);
    }

    #[test]
    fn folds_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let state = || StateDir::new(tmp.path().into());
        let mut a = app().with_state(state());
        a.handle_key(key(' '));
        let b = app().with_state(state());
        assert!(groups(&b)[0].folded);
        assert!(tmp.path().join(FOLDS_FILE).exists());
    }

    #[test]
    fn cursor_survives_toggle_and_refresh() {
        let mut a = app();
        select(&mut a, 91);
        a.handle_key(key('a'));
        a.handle_key(key('a'));
        assert_eq!(a.selected_pr().unwrap().pr.number, 91);
        a.handle_key(key('g'));
        assert_eq!(a.selected_pr().unwrap().pr.number, 91);
        a.handle_key(key('g'));
        select(&mut a, 418);
        let last = a.screen(now()).cursor;
        a.set_cache(cache_with(|n| n != 418));
        let s = a.screen(now());
        let ReviewBody::Groups(gs) = &s.body else {
            panic!()
        };
        let lines: usize = gs.iter().map(|g| 1 + g.rows.len()).sum();
        assert_eq!(s.cursor, lines - 1);
        assert!(s.cursor < last);
    }

    #[test]
    fn not_configured_and_invalid() {
        let s = ReviewApp::new(ReviewConfig::Missing {
            path: "/c.toml".into(),
        })
        .screen(now());
        assert_eq!(
            s.body,
            ReviewBody::NotConfigured {
                path: "/c.toml".into()
            }
        );
        assert_eq!(s.banner, None);
        let s = ReviewApp::new(ReviewConfig::Invalid {
            path: "/c.toml".into(),
            message: "bad".into(),
        })
        .screen(now());
        let banner = s.banner.unwrap();
        assert_eq!(banner.message, "config: bad");
        assert_eq!(banner.fix, "Fix /c.toml, then relaunch the pane.");
    }

    #[test]
    fn empty_counts_the_other_filter() {
        let mut a = app();
        a.set_cache(cache_with(|n| n == 421 || n == 431));
        let s = a.screen(now());
        assert_eq!(
            s.body,
            ReviewBody::Empty {
                repos: 3,
                show: Show::NotApproved,
                other: 2
            }
        );
        assert_eq!(s.repos, 3);
    }

    #[test]
    fn teams_missing_drops_stars_and_flags_notice() {
        let mut a = app();
        let mut c = cache_with(|_| true);
        c.teams = None;
        a.set_cache(c);
        let s = a.screen(now());
        assert_eq!(s.code_owner, 0);
        assert!(s.teams_missing);
        assert!(!app().screen(now()).teams_missing);
    }

    #[test]
    fn error_keeps_rows_and_sets_banner() {
        let mut a = app();
        let mut c = cache_with(|_| true);
        c.error = Some(GhError::Other(
            "The listed users and repositories cannot be searched".into(),
        ));
        a.set_cache(c);
        let s = a.screen(now());
        assert!(s.stale);
        assert_eq!(s.prs, 9);
        assert!(s.banner.unwrap().message.contains("cannot be searched"));
    }

    const NOT_SEARCHABLE: &str = "gh: The listed users and repositories cannot be searched \
         either because the resources do not exist or you do not have permission to view them.";

    #[test]
    fn unsearchable_repo_points_at_the_config() {
        let mut a = ReviewApp::new(ReviewConfig::Repos(vec!["heard/nope".into()]))
            .with_config_path("/c.toml".into());
        a.apply(ReviewEvent::Done(ReviewCache {
            error: Some(GhError::Other(NOT_SEARCHABLE.into())),
            ..ReviewCache::default()
        }));
        let banner = a.screen(now()).banner.unwrap();
        assert!(banner.message.contains("cannot be searched"));
        assert_eq!(
            banner.fix,
            "Check the repos in /c.toml, then relaunch the pane."
        );
        // Other errors keep gh's own fix.
        a.apply(ReviewEvent::Done(ReviewCache {
            error: Some(GhError::LoginExpired),
            ..ReviewCache::default()
        }));
        assert_eq!(
            a.screen(now()).banner.unwrap().fix,
            GhError::LoginExpired.fix()
        );
    }

    #[test]
    fn loading_before_data() {
        let mut a = ReviewApp::new(ReviewConfig::Repos(vec!["heard/a".into()]));
        assert_eq!(
            a.screen(now()).body,
            ReviewBody::Loading(LoadProgress::default())
        );
        a.apply(ReviewEvent::Started);
        assert!(a.screen(now()).fetching);
        let p = vec![("heard/a".to_string(), RepoStatus::Loading)];
        a.apply(ReviewEvent::Progress(p.clone()));
        assert_eq!(
            a.screen(now()).body,
            ReviewBody::Loading(LoadProgress { repos: p })
        );
        // A failure before any data keeps loading, with the banner.
        a.apply(ReviewEvent::Done(ReviewCache {
            error: Some(GhError::LoginExpired),
            ..ReviewCache::default()
        }));
        let s = a.screen(now());
        assert!(matches!(s.body, ReviewBody::Loading(_)));
        assert!(s.banner.is_some());
        assert!(!s.fetching);
    }

    #[test]
    fn repos_lists_configured_only() {
        assert_eq!(app().repos().len(), 3);
        assert!(
            ReviewApp::new(ReviewConfig::Missing { path: "/c".into() })
                .repos()
                .is_empty()
        );
    }
}
