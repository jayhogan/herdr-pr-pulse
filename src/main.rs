#![allow(dead_code)] // removed in Task 21

mod ago;
mod app;
mod attention;
mod fetch;
mod git;
mod github;
mod herdr;
mod model;
mod stacks;
mod store;
mod ui;
mod workspaces;

use std::env;
use std::io;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Offset, Utc};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::app::{Action, App, ViewKind};
use crate::fetch::{FetchEvent, FetchRequest, Poller};
use crate::git::{FixtureBranches, GitBranches};
use crate::github::GhCli;
use crate::store::StateDir;

const USAGE: &str = "usage: herdr-pr-pulse [workspace|mine] [--fixtures]";
const TICK: Duration = Duration::from_secs(1);
const SNAPSHOT_EVERY: Duration = Duration::from_secs(2);

const SEARCH: &str = include_str!("../tests/fixtures/search.json");
const DETAIL: &str = include_str!("../tests/fixtures/detail.json");
const SNAPSHOT: &str = include_str!("../tests/fixtures/snapshot.json");
const WORKSPACES: &str = include_str!("../tests/fixtures/workspaces.json");
const FIXTURE_FETCHED_AT: &str = "2026-10-17T14:32:08Z";

#[derive(Debug, PartialEq)]
struct Args {
    kind: ViewKind,
    fixtures: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut kind = None;
    let mut fixtures = false;
    for arg in args {
        let this = match arg.as_str() {
            "--fixtures" => {
                fixtures = true;
                continue;
            }
            "workspace" => ViewKind::Workspace,
            "mine" => ViewKind::Mine,
            other => return Err(format!("unrecognized argument '{other}'")),
        };
        if kind.replace(this).is_some() {
            return Err("view given more than once".into());
        }
    }
    Ok(Args {
        kind: kind.unwrap_or(ViewKind::Workspace),
        fixtures,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("herdr-pr-pulse: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("herdr-pr-pulse: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let app = App::new(args.kind, Local::now().offset().fix());
    // Folds persist in the real state dir; fixtures mode keeps them in a throwaway one
    // (created on the first fold, removed on exit).
    let fixtures_dir = env::temp_dir().join(format!("herdr-pr-pulse-folds-{}", std::process::id()));
    let mut app = app.with_state(if args.fixtures {
        StateDir::new(fixtures_dir.clone())
    } else {
        StateDir::default_location()
    });
    let mut live = if args.fixtures {
        load_fixtures(&mut app)?;
        None
    } else {
        let in_herdr = env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        let pane = env::var("HERDR_PANE_ID").ok().filter(|p| !p.is_empty());
        let (true, Some(pane)) = (in_herdr, pane) else {
            return Err("not running inside a herdr pane".into());
        };
        app.set_own_pane(pane);
        Some(Live::start())
    };
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, live.as_mut());
    ratatui::restore();
    if args.fixtures {
        let _ = std::fs::remove_dir_all(&fixtures_dir);
    }
    result.map_err(|e| e.to_string())
}

fn load_fixtures(app: &mut App) -> Result<(), String> {
    let search = model::parse_search(SEARCH)?;
    let prs = model::parse_detail(DETAIL)?;
    let ids = |hits: &[model::SearchHit]| hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>();
    let fetched_at = DateTime::parse_from_rfc3339(FIXTURE_FETCHED_AT)
        .map_err(|e| e.to_string())?
        .with_timezone(&Utc);
    app.set_prs(
        prs,
        &ids(&search.authored),
        &ids(&search.assigned),
        &search.viewer,
        fetched_at,
    );

    // Agents from the fixture snapshot, as seen from the pane each view lives in. Memory
    // starts from the fixture file in a throwaway dir, never the real state dir.
    app.set_own_pane(match app.kind() {
        ViewKind::Workspace => "w1:p2".into(),
        ViewKind::Mine => "w4:p1".into(),
    });
    app.set_path_exists(|_| true);
    // Unique per call: tests load fixtures concurrently within one process.
    static LOADS: AtomicUsize = AtomicUsize::new(0);
    let n = LOADS.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "herdr-pr-pulse-fixtures-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let seeded = std::fs::write(dir.join(workspaces::FILE), WORKSPACES);
    if let Err(e) = seeded {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("{}: {e}", dir.display()));
    }
    app.apply_snapshot(
        herdr::parse_snapshot(SNAPSHOT),
        &mut FixtureBranches::embedded(),
        &StateDir::new(dir.clone()),
        fetched_at,
    );
    let _ = std::fs::remove_dir_all(&dir); // the app keeps what it loaded
    Ok(())
}

/// The background GitHub fetch, the herdr snapshot poll, and when to ask each for data.
/// Fixtures mode has none.
struct Live {
    requests: Sender<FetchRequest>,
    events: Receiver<FetchEvent>,
    poller: Poller,
    state: StateDir,
    branches: GitBranches,
    next_snapshot: Instant,
}

impl Live {
    fn start() -> Live {
        let (requests, events) = fetch::spawn(Box::new(GhCli), StateDir::default_location());
        Live {
            requests,
            events,
            poller: Poller::new(Instant::now()),
            state: StateDir::default_location(),
            branches: GitBranches::default(),
            next_snapshot: Instant::now(),
        }
    }

    /// Reads herdr every 2 s, sends due requests and applies every event that has arrived.
    fn tick(&mut self, app: &mut App) {
        if Instant::now() >= self.next_snapshot {
            let snap = herdr::fetch_snapshot();
            app.apply_snapshot(snap, &mut self.branches, &self.state, Utc::now());
            self.next_snapshot = Instant::now() + SNAPSHOT_EVERY;
        }
        for req in self.poller.due(Instant::now()) {
            let _ = self.requests.send(req); // a dead worker leaves the last data on screen
        }
        while let Ok(ev) = self.events.try_recv() {
            let done = match &ev {
                FetchEvent::Done(cache) => Some(cache.fetched_at),
                _ => None,
            };
            app.apply_fetch(ev, Utc::now());
            if let Some(fetched_at) = done {
                self.poller
                    .on_done(Instant::now(), fetched_at, app.unknown_merge_ids());
            }
        }
    }

    fn refresh(&mut self) {
        let _ = self.requests.send(self.poller.forced());
    }
}

/// Carries out a key's action; true when the app should quit. Fixtures mode (`live` is
/// `None`) only says what it would do.
fn run_action(app: &mut App, action: Action, live: Option<&mut Live>) -> bool {
    match (action, live) {
        (Action::Quit, _) => return true,
        (Action::None, _) => {}
        (Action::Refresh, Some(live)) => live.refresh(),
        (Action::Refresh, None) => {}
        (Action::Focus(pane), Some(_)) => {
            if let Err(e) = herdr::focus_pane(&pane) {
                app.set_status(e);
            }
        }
        (Action::Focus(pane), None) => app.set_status(format!("fixtures: would focus {pane}")),
        (Action::Open(url), Some(_)) => {
            if let Err(e) = open_in_browser(&url) {
                app.set_status(e);
            }
        }
        (Action::Open(url), None) => app.set_status(format!("fixtures: would open {url}")),
    }
    false
}

/// `gh pr view <url> --web`, not waited on by the UI: a detached thread reaps the child.
fn open_in_browser(url: &str) -> Result<(), String> {
    let mut child = Command::new("gh")
        .args(["pr", "view", url, "--web"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run gh: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut live: Option<&mut Live>,
) -> io::Result<()> {
    let mut scroll = 0;
    loop {
        if let Some(live) = live.as_deref_mut() {
            live.tick(app);
        }
        let screen = app.screen(Utc::now());
        terminal.draw(|frame| ui::rows::render(frame, &screen, &mut scroll))?;

        // A key or resize wakes the loop early; otherwise redraw each tick.
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let action = app.handle_key(key);
            if run_action(app, action, live.as_deref_mut()) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn defaults_to_workspace() {
        assert_eq!(
            parse(&[]),
            Ok(Args {
                kind: ViewKind::Workspace,
                fixtures: false
            })
        );
    }

    #[test]
    fn mine_positional() {
        assert_eq!(parse(&["mine"]).unwrap().kind, ViewKind::Mine);
    }

    #[test]
    fn flag_and_positional_in_any_order() {
        let want = Ok(Args {
            kind: ViewKind::Mine,
            fixtures: true,
        });
        assert_eq!(parse(&["--fixtures", "mine"]), want);
        assert_eq!(parse(&["mine", "--fixtures"]), want);
        assert_eq!(
            parse(&["--fixtures"]),
            Ok(Args {
                kind: ViewKind::Workspace,
                fixtures: true
            })
        );
    }

    #[test]
    fn rejects_unknown_and_duplicates() {
        assert!(parse(&["bogus"]).is_err());
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["mine", "workspace"]).is_err());
        assert!(parse(&["mine", "mine"]).is_err());
    }

    #[test]
    fn fixtures_load() {
        let mut app = App::new(ViewKind::Mine, Utc::now().offset().fix());
        load_fixtures(&mut app).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-17T14:32:26Z")
            .unwrap()
            .with_timezone(&Utc);
        let app::Body::Groups(gs) = app.screen(now).body else {
            panic!("expected groups")
        };
        let audit = gs.iter().find(|g| g.name == "audit").unwrap();
        assert_eq!(audit.agent.as_ref().unwrap().pane_id, "w1:p1");
        assert_eq!(audit.rank, attention::Attention::Urgent);
        assert_eq!(app.screen(now).header.branch, None);

        let mut ws = App::new(ViewKind::Workspace, Utc::now().offset().fix());
        load_fixtures(&mut ws).unwrap();
        assert_eq!(
            ws.screen(now).header.branch.as_deref(),
            Some("audit/kafka-producer")
        );
    }

    #[test]
    fn fixtures_actions_only_set_status() {
        let mut app = App::new(ViewKind::Workspace, Utc::now().offset().fix());
        load_fixtures(&mut app).unwrap();
        assert!(run_action(&mut app, Action::Quit, None));
        assert!(!run_action(&mut app, Action::Focus("w1:p1".into()), None));
        assert_eq!(
            app.screen(Utc::now()).status.as_deref(),
            Some("fixtures: would focus w1:p1")
        );
        assert!(!run_action(
            &mut app,
            Action::Open("https://x/1".into()),
            None
        ));
        assert_eq!(
            app.screen(Utc::now()).status.as_deref(),
            Some("fixtures: would open https://x/1")
        );
        assert!(!run_action(&mut app, Action::Refresh, None));
    }
}
