#![allow(dead_code)] // removed in Task 21

mod ago;
mod app;
mod attention;
mod github;
mod model;
mod stacks;
mod ui;

use std::env;
use std::io;
use std::process::ExitCode;
use std::time::Duration;

use chrono::{DateTime, Local, Offset, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::app::{App, ViewKind};

const USAGE: &str = "usage: herdr-pr-pulse [workspace|mine] [--fixtures]";
const TICK: Duration = Duration::from_secs(1);

const SEARCH: &str = include_str!("../tests/fixtures/search.json");
const DETAIL: &str = include_str!("../tests/fixtures/detail.json");
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
    if !args.fixtures {
        let in_herdr = env::var("HERDR_ENV").is_ok_and(|v| v == "1");
        let pane = env::var("HERDR_PANE_ID").ok().filter(|p| !p.is_empty());
        if !in_herdr || pane.is_none() {
            return Err("not running inside a herdr pane".into());
        }
    }
    let mut app = App::new(args.kind, Local::now().offset().fix());
    if args.fixtures {
        load_fixtures(&mut app)?;
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
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
    Ok(())
}

/// Returns true when the app should quit. Task 14 moves this into `App::handle_key`.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1),
        _ => {}
    }
    false
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut scroll = 0;
    loop {
        let screen = app.screen(Utc::now());
        terminal.draw(|frame| ui::rows::render(frame, &screen, &mut scroll))?;

        // A key or resize wakes the loop early; otherwise redraw each tick.
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key(app, key.code, key.modifiers)
        {
            return Ok(());
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
        assert!(matches!(app.screen(now).body, app::Body::Groups(_)));
    }

    #[test]
    fn keys_quit_and_move() {
        let mut app = App::new(ViewKind::Mine, Utc::now().offset().fix());
        load_fixtures(&mut app).unwrap();
        assert!(handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(handle_key(
            &mut app,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert!(!handle_key(
            &mut app,
            KeyCode::Char('c'),
            KeyModifiers::NONE
        ));
        assert!(!handle_key(
            &mut app,
            KeyCode::Char('j'),
            KeyModifiers::NONE
        ));
        assert_eq!(app.screen(Utc::now()).cursor, 1);
        assert!(!handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.screen(Utc::now()).cursor, 0);
    }
}
