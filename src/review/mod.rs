//! The Review view: other people's ready PRs in the configured repos, waiting on review.

pub mod app;
pub mod fetch;
pub mod github;
pub mod model;

use std::io;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::app::Action;
use crate::config;
use crate::fetch::{FetchRequest, Poller};
use crate::github::GhCli;
use crate::review::app::{ReviewApp, fixture_app};
use crate::review::fetch::{ReviewCache, ReviewEvent};
use crate::store::StateDir;
use crate::ui;

const TICK: Duration = Duration::from_secs(1);

const SEARCH: &str = include_str!("../../tests/fixtures/review-search.json");
const DETAIL: &str = include_str!("../../tests/fixtures/review-detail.json");
const TEAMS: &str = include_str!("../../tests/fixtures/review-teams.json");
const FIXTURE_FETCHED_AT: &str = "2026-10-20T15:00:00Z";

/// The configured repos in fixtures mode.
pub const FIXTURE_REPOS: [&str; 3] = ["heard/heard-ts", "heard/tax-engine", "heard/docket"];

/// The cache a fetch of the fixture files would write.
pub fn fixture_cache() -> Result<ReviewCache, String> {
    let search = model::parse_search_page(SEARCH)?;
    let teams = model::parse_teams(TEAMS)?;
    let fetched_at: DateTime<Utc> = FIXTURE_FETCHED_AT.parse().map_err(|e| format!("{e}"))?;
    Ok(ReviewCache {
        version: 1,
        fetched_at: Some(fetched_at),
        viewer: search.viewer,
        repos: FIXTURE_REPOS.map(String::from).to_vec(),
        teams: Some([("heard".to_string(), teams)].into()),
        prs: model::parse_review_detail(DETAIL)?,
        error: None,
        error_at: None,
    })
}

/// Runs the Review view until `q`. It needs no herdr pane: it has no herdr features.
pub fn run(fixtures: bool) -> Result<(), String> {
    let (mut app, mut live) = if fixtures {
        (fixture_app(), None)
    } else {
        let app = ReviewApp::new(config::load_review())
            .with_config_path(config::default_path())
            .with_state(StateDir::default_location());
        let live = (!app.repos().is_empty()).then(|| Live::start(app.repos()));
        (app, live)
    };
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, live.as_mut());
    ratatui::restore();
    result.map_err(|e| e.to_string())
}

/// The background fetch and when to ask it for data. Fixtures mode, or a config with no
/// repos, has none.
struct Live {
    requests: Sender<FetchRequest>,
    events: Receiver<ReviewEvent>,
    poller: Poller,
}

impl Live {
    fn start(repos: Vec<String>) -> Live {
        let (requests, events) = fetch::spawn(Box::new(GhCli), StateDir::default_location(), repos);
        Live {
            requests,
            events,
            poller: Poller::new(Instant::now()),
        }
    }

    /// Sends due requests and applies every event that has arrived.
    fn tick(&mut self, app: &mut ReviewApp) {
        for req in self.poller.due(Instant::now()) {
            let _ = self.requests.send(req); // a dead worker leaves the last data on screen
        }
        while let Ok(ev) = self.events.try_recv() {
            let done = match &ev {
                ReviewEvent::Done(cache) => Some(cache.fetched_at),
                _ => None,
            };
            app.apply(ev);
            if let Some(fetched_at) = done {
                self.poller.on_done(Instant::now(), fetched_at, Vec::new());
            }
        }
    }
}

/// Carries out a key's action; true when the app should quit. Without `live` (fixtures, or
/// nothing configured) it only says what it would do.
fn run_action(app: &mut ReviewApp, action: Action, live: Option<&mut Live>) -> bool {
    match (action, live) {
        (Action::Quit, _) => return true,
        (Action::Refresh, Some(live)) => {
            let _ = live.requests.send(live.poller.forced());
        }
        (Action::Open(url), Some(_)) => {
            if let Err(e) = crate::open_in_browser(&url) {
                app.set_status(e);
            }
        }
        (Action::Open(url), None) => app.set_status(format!("fixtures: would open {url}")),
        _ => {}
    }
    false
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut ReviewApp,
    mut live: Option<&mut Live>,
) -> io::Result<()> {
    let mut scroll = 0;
    loop {
        if let Some(live) = live.as_deref_mut() {
            live.tick(app);
        }
        app.advance_spinner();
        let screen = app.screen(Utc::now());
        let mut help_max = 0;
        terminal.draw(|frame| help_max = ui::review::render(frame, &screen, &mut scroll))?;
        if screen.help {
            app.set_help_max(help_max);
        }
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
    use crate::review::app::ReviewBody;

    #[test]
    fn fixtures_load() {
        let now: DateTime<Utc> = FIXTURE_FETCHED_AT.parse().unwrap();
        let s = fixture_app().screen(now);
        assert_eq!(s.prs, 9);
        let ReviewBody::Groups(gs) = s.body else {
            panic!("expected groups")
        };
        assert_eq!(gs.len(), 5);
    }

    #[test]
    fn fixtures_actions_only_set_status() {
        let mut app = fixture_app();
        assert!(run_action(&mut app, Action::Quit, None));
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
