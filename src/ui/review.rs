//! The Review view's screen (review design §6): header, summary, groups of PR rows with
//! their details, and key hints.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ago;
use crate::app::{Banner, LoadProgress};
use crate::fetch::RepoStatus;
use crate::review::app::{GroupBy, ReviewBody, ReviewGroup, ReviewRow, ReviewScreen, Show};
use crate::ui::cells::{cell, dw, fit, plural};
use crate::ui::drawer::review_drawer_lines;
use crate::ui::rows::Density::{self, *};
use crate::ui::rows::density;
use crate::ui::{help, theme};

/// One-line row columns: cursor, marks, #, then (after the title) repo, user, ready.
const WIDE: [usize; 6] = [2, 3, 7, 14, 14, 6];
/// Medium drops the column the rows are grouped by.
const MEDIUM: [usize; 5] = [2, 3, 7, 13, 6];

fn short_repo(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

fn dim_all(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| Span::styled(s.content, theme::dim()))
        .collect()
}

/// `spans` cut to `width` columns, the last one ending in `…` when cut.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = s.width();
        if used + w <= width {
            used += w;
            out.push(s);
        } else {
            out.push(Span::styled(fit(&s.content, width - used), s.style));
            break;
        }
    }
    Line::from(out)
}

/// Draws the screen and returns, while the help overlay is open, how far it can scroll
/// (else 0); same contract as `rows::render`.
pub fn render(frame: &mut Frame, screen: &ReviewScreen, scroll: &mut usize) -> usize {
    let area = frame.area();
    let d = density(area.width);
    let width = area.width as usize;
    let top = top_lines(screen, d, width);
    let bottom = footer(screen, d, width);
    let (body, cursor, cursor_end) = if screen.help {
        (help::review_help_lines(area.width), 0, 0)
    } else {
        body_lines(screen, d, area.width)
    };
    let top_h = (top.len() as u16).min(area.height);
    let bottom_h = (bottom.len() as u16).min(area.height.saturating_sub(top_h));
    let body_h = area.height.saturating_sub(top_h + bottom_h);
    let h = body_h as usize;
    if h > 0 && !screen.help {
        if cursor < *scroll {
            *scroll = cursor;
        } else if cursor_end >= *scroll + h {
            *scroll = (cursor_end + 1).saturating_sub(h).min(cursor);
        }
    }
    let help_max = if screen.help {
        body.len().saturating_sub(h)
    } else {
        0
    };
    let scrolled = if screen.help {
        screen.help_scroll.min(help_max)
    } else {
        *scroll = (*scroll).min(body.len().saturating_sub(h));
        *scroll
    };
    let rect = |y: u16, h: u16| Rect::new(area.x, area.y + y, area.width, h);
    frame.render_widget(Paragraph::new(top), rect(0, top_h));
    let scrolled = u16::try_from(scrolled).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(body).scroll((scrolled, 0)),
        rect(top_h, body_h),
    );
    frame.render_widget(Paragraph::new(bottom), rect(top_h + body_h, bottom_h));
    help_max
}

// ---------------------------------------------------------------- top and bottom

fn top_lines(s: &ReviewScreen, d: Density, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![header_bar(s, d, width)];
    match &s.banner {
        Some(b) => out.extend(banner_lines(b, width)),
        None => out.push(summary(s, d, width)),
    }
    if s.teams_missing {
        let t = " ★ off: can't read your teams · gh auth refresh -s read:org";
        out.push(Line::styled(fit(t, width), theme::dim()));
    }
    if matches!(d, Wide | Medium) {
        out.push(Line::styled("─".repeat(width), theme::dim()));
        if !s.help && matches!(s.body, ReviewBody::Groups(_)) {
            out.push(column_titles(s, d, width));
        }
    }
    out
}

fn header_bar(s: &ReviewScreen, d: Density, width: usize) -> Line<'static> {
    let group = match s.group_by {
        GroupBy::User => "user",
        GroupBy::Repo => "repo",
    };
    let show = match s.show {
        Show::NotApproved => "not approved",
        Show::Approved => "approved",
    };
    let listed = matches!(s.body, ReviewBody::Groups(_));
    let left = match d {
        Wide if listed => format!(
            "· Review │ {} · {} · {}",
            plural(s.prs, "PR", "PRs"),
            plural(s.people, "person", "people"),
            plural(s.repos, "repo", "repos")
        ),
        Medium if listed => format!("· Review │ {}", plural(s.prs, "PR", "PRs")),
        _ => "· Review".into(),
    };
    let spin = if s.fetching { s.spinner } else { "↻" };
    let age = s.fetched_at.map(|t| ago::short(t, s.now));
    let state = match age {
        _ if s.stale => {
            let busy = if s.fetching {
                format!("{} ", s.spinner)
            } else {
                String::new()
            };
            let age = age.map_or(String::new(), |a| format!(" · stale {a}"));
            format!("{busy}✗ offline{age} ")
        }
        None if s.fetching => format!("{} loading ", s.spinner),
        None => String::new(),
        Some(age) => format!("{spin} {age} "),
    };
    let sep = if state.is_empty() { " " } else { " │ " };
    let right = match d {
        Wide => format!("group: {group}  show: {show}{sep}{state}"),
        Medium => format!("{group} · {show}{sep}{state}"),
        Narrow | Tiny => state,
    };
    let title = " PR Pulse ";
    let room = width.saturating_sub(dw(title) + dw(&right));
    let left = fit(&left, room);
    let gap = room.saturating_sub(dw(&left));
    Line::from(vec![
        Span::styled(fit(title, width), theme::bold()),
        Span::raw(format!("{left}{}{right}", " ".repeat(gap))),
    ])
    .style(theme::bar())
}

fn summary(s: &ReviewScreen, d: Density, width: usize) -> Line<'static> {
    let waiting = if matches!(d, Wide | Medium) {
        " waiting on author"
    } else {
        " waiting"
    };
    let mut spans = vec![Span::raw(" ")];
    if s.code_owner > 0 {
        spans.push(Span::styled(format!("★{}", s.code_owner), theme::yellow()));
        spans.push(Span::styled(" code owner  ", theme::dim()));
    }
    if s.waiting > 0 {
        spans.push(Span::styled(
            format!("↩{}{waiting}", s.waiting),
            theme::dim(),
        ));
    }
    clip(spans, width)
}

/// The red banner: what went wrong and `r retry`, then the fix.
fn banner_lines(b: &Banner, width: usize) -> Vec<Line<'static>> {
    let right = "r retry ";
    let left = fit(
        &format!(" ✗ {}.", b.message),
        width.saturating_sub(dw(right) + 1),
    );
    let gap = width.saturating_sub(dw(&left) + dw(right));
    vec![
        Line::raw(format!("{left}{}{right}", " ".repeat(gap))).style(theme::hot_row()),
        Line::raw(fit(&format!("   Fix: {}", b.fix), width)),
    ]
}

/// Which optional column each density shows: (repo, user).
fn columns(s: &ReviewScreen, d: Density) -> (bool, bool) {
    match d {
        Wide => (true, true),
        _ => (s.group_by == GroupBy::User, s.group_by == GroupBy::Repo),
    }
}

fn column_titles(s: &ReviewScreen, d: Density, width: usize) -> Line<'static> {
    let style = theme::dim();
    let (repo, user) = columns(s, d);
    let (w, other, ready) = match d {
        Wide => (&WIDE[..], 0, WIDE[5]),
        _ => (&MEDIUM[..], MEDIUM[3], MEDIUM[4]),
    };
    let mut spans = vec![
        cell("", w[0], style),
        cell("", w[1], style),
        cell("#", w[2], style),
    ];
    spans.push(cell("TITLE", title_width(d, width), style));
    match d {
        Wide => {
            spans.push(cell("REPO", WIDE[3], style));
            spans.push(cell("USER", WIDE[4], style));
        }
        _ if repo => spans.push(cell("REPO", other, style)),
        _ if user => spans.push(cell("USER", other, style)),
        _ => {}
    }
    spans.push(cell("READY", ready, style));
    Line::from(spans)
}

fn title_width(d: Density, width: usize) -> usize {
    let fixed: usize = match d {
        Wide => WIDE.iter().sum(),
        _ => MEDIUM.iter().sum(),
    };
    width.saturating_sub(fixed).max(10)
}

fn footer(s: &ReviewScreen, d: Density, width: usize) -> Vec<Line<'static>> {
    let keys: &[(&str, &str)] = if s.help {
        &[
            (" j/k", " scroll  "),
            ("? or Esc", " close  "),
            ("q", " quit"),
        ]
    } else {
        match d {
            Wide => &[
                (" j/k", " move  "),
                ("␣", " details/fold  "),
                ("o", " open  "),
                ("r", " refresh  "),
                ("g", " group  "),
                ("a", " approved⇄not  "),
                ("?", " help"),
            ],
            Medium => &[
                (" j/k", ""),
                (" ␣", " info "),
                ("o", " open "),
                ("r", " ↻ "),
                ("g", " group "),
                ("a", " appr "),
                ("?", " help"),
            ],
            Narrow => &[
                ("j/k", " move "),
                ("␣", " info "),
                ("o", " open "),
                ("?", " keys"),
            ],
            Tiny => &[
                ("j/k", ""),
                (" ␣", " info"),
                (" o", " open"),
                (" ?", " keys"),
            ],
        }
    };
    let spans: Vec<Span> = keys
        .iter()
        .flat_map(|(k, what)| {
            [
                Span::styled(k.to_string(), theme::dim().patch(theme::bold())),
                Span::styled(what.to_string(), theme::dim()),
            ]
        })
        .collect();
    let hints = match s.status.as_ref().filter(|_| !s.help) {
        Some(status) => Line::raw(fit(&format!(" {status}"), width)),
        None => Line::from(spans),
    };
    match d {
        Wide | Medium => vec![Line::styled("─".repeat(width), theme::dim()), hints],
        Narrow | Tiny => vec![hints],
    }
}

// ---------------------------------------------------------------- body

/// Body lines with the cursor's first and last line (rows can take two lines, plus
/// details).
fn body_lines(s: &ReviewScreen, d: Density, width: u16) -> (Vec<Line<'static>>, usize, usize) {
    let w = width as usize;
    let groups = match &s.body {
        ReviewBody::Loading(p) => return (loading_lines(s, p, w), 0, 0),
        ReviewBody::NotConfigured { path } => return (not_configured(path, w), 0, 0),
        ReviewBody::Empty { repos, show, other } => {
            return (empty_lines(*repos, *show, *other, w), 0, 0);
        }
        ReviewBody::Groups(gs) => gs,
    };
    let mut out = Vec::new();
    let (mut cursor_line, mut cursor_end) = (0, 0);
    let mut selectable = 0;
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            out.push(Line::raw(""));
        }
        if selectable == s.cursor {
            (cursor_line, cursor_end) = (out.len(), out.len());
        }
        out.push(group_header(g, selectable == s.cursor, s, w));
        selectable += 1;
        if g.folded {
            continue;
        }
        for r in &g.rows {
            let cur = selectable == s.cursor;
            let lines = row_lines(s, r, d, w, cur);
            if cur {
                (cursor_line, cursor_end) = (out.len(), out.len() + lines.len() - 1);
            }
            out.extend(lines);
            if s.drawer_pr.as_deref() == Some(r.pr.pr.id.as_str()) {
                out.extend(review_drawer_lines(r, &s.viewer, width, s.now));
                if cur {
                    cursor_end = out.len() - 1;
                }
            }
            selectable += 1;
        }
    }
    (out, cursor_line, cursor_end)
}

fn group_header(g: &ReviewGroup, cursor: bool, s: &ReviewScreen, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    if cursor {
        spans.push(Span::styled("▶", theme::bold()));
    }
    let fold = if g.folded { "▸ " } else { "▾ " };
    spans.push(Span::styled(fold, theme::bold()));
    spans.push(Span::styled(g.name.clone(), theme::bold()));
    spans.push(Span::styled(
        format!(
            "  {} · oldest {}",
            plural(g.rows.len(), "PR", "PRs"),
            ago::short(g.oldest, s.now)
        ),
        theme::dim(),
    ));
    let spans = if s.stale { dim_all(spans) } else { spans };
    clip(spans, width)
}

fn marks(r: &ReviewRow) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    if r.code_owner {
        out.push(Span::styled("★", theme::yellow()));
    }
    if r.waiting {
        out.push(Span::styled("↩", theme::dim()));
    }
    out
}

fn row_lines(
    s: &ReviewScreen,
    r: &ReviewRow,
    d: Density,
    width: usize,
    cur: bool,
) -> Vec<Line<'static>> {
    let pr = &r.pr.pr;
    let dim = s.stale || r.waiting;
    let base = if dim { theme::dim() } else { Style::new() };
    let text = if cur { base.patch(theme::bold()) } else { base };
    let cursor = if cur { "▶" } else { " " };
    let number = format!("#{}", pr.number);
    let repo = short_repo(&pr.repo);
    let age = ago::short(r.pr.ready_at, s.now);
    let mut marks = marks(r);
    if s.stale {
        marks = dim_all(marks);
    }
    let marks_w: usize = marks.iter().map(Span::width).sum();
    let (show_repo, show_user) = columns(s, d);
    match d {
        Wide | Medium => {
            let w = if d == Wide { &WIDE[..] } else { &MEDIUM[..] };
            let mut spans = vec![Span::styled(
                cell(cursor, w[0], text).content,
                theme::bold(),
            )];
            spans.extend(marks);
            spans.push(Span::raw(" ".repeat(w[1].saturating_sub(marks_w))));
            spans.push(cell(&number, w[2], text));
            spans.push(cell(&pr.title, title_width(d, width), text));
            if d == Wide {
                spans.push(cell(repo, WIDE[3], base));
                spans.push(cell(&pr.author, WIDE[4], base));
                spans.push(cell(&age, WIDE[5], base));
            } else {
                let other = if show_repo { repo } else { &pr.author };
                if show_repo || show_user {
                    spans.push(cell(other, MEDIUM[3], base));
                }
                spans.push(cell(&age, MEDIUM[4], base));
            }
            vec![clip(spans, width)]
        }
        Narrow | Tiny => {
            let mut first = vec![Span::styled(cursor, theme::bold())];
            first.extend(marks);
            first.push(Span::raw(" ".repeat(2usize.saturating_sub(marks_w) + 1)));
            first.push(Span::styled(format!("{number} "), text));
            first.push(Span::styled(pr.title.clone(), text));
            let other = if show_repo { repo } else { &pr.author };
            let second = vec![
                Span::raw("     "),
                Span::styled(other.to_string(), base),
                Span::styled(format!(" · {age}"), theme::dim()),
            ];
            vec![clip(first, width), clip(second, width)]
        }
    }
}

// ---------------------------------------------------------------- states

fn loading_lines(s: &ReviewScreen, p: &LoadProgress, width: usize) -> Vec<Line<'static>> {
    if s.banner.is_some() && !s.fetching {
        let spans = vec![
            Span::styled(" ✗ ", theme::red_bold()),
            Span::styled("No PR data yet.", theme::bold()),
            Span::styled("  Press r to retry.", theme::dim()),
        ];
        return vec![clip(spans, width)];
    }
    let spin = theme::cyan().patch(theme::bold());
    let mut out = vec![clip(
        vec![
            Span::styled(format!(" {} ", s.spinner), spin),
            Span::raw("Loading PRs waiting on review…"),
        ],
        width,
    )];
    for (repo, status) in &p.repos {
        let (glyph, style, note) = match status {
            RepoStatus::Done(n) => ("✓", theme::green(), plural(*n, "PR", "PRs")),
            RepoStatus::Loading => (s.spinner, spin, String::new()),
            RepoStatus::Queued => ("·", theme::dim(), String::new()),
        };
        out.push(clip(
            vec![
                Span::styled(format!("   {glyph} "), style),
                Span::raw(repo.clone()),
                Span::styled(format!("  {note}"), theme::dim()),
            ],
            width,
        ));
    }
    out
}

fn not_configured(path: &str, width: usize) -> Vec<Line<'static>> {
    [
        ("  No repos configured.", theme::bold()),
        ("", Style::new()),
        (&*format!("  Add them to {path}:"), Style::new()),
        ("", Style::new()),
        ("    [review]", theme::dim()),
        (
            "    repos = [\"owner/name\", \"owner/other\"]",
            theme::dim(),
        ),
        ("", Style::new()),
        ("  then relaunch this pane.", Style::new()),
    ]
    .into_iter()
    .map(|(t, style)| Line::styled(fit(t, width), style))
    .collect()
}

fn empty_lines(repos: usize, show: Show, other: usize, width: usize) -> Vec<Line<'static>> {
    let in_repos = plural(repos, "repo", "repos");
    let (text, hint) = match show {
        Show::NotApproved => (
            format!("  Nothing waiting on review in {in_repos}."),
            "show approved",
        ),
        Show::Approved => (
            format!("  No approved PRs in {in_repos}."),
            "show not approved",
        ),
    };
    let mut out = vec![Line::styled(fit(&text, width), theme::bold())];
    if other > 0 {
        out.push(clip(
            vec![
                Span::styled("  a", theme::bold()),
                Span::styled(format!(" {hint} ({other})"), theme::dim()),
            ],
            width,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::config::ReviewConfig;
    use crate::review::app::{ReviewApp, fixture_app};

    fn now() -> DateTime<Utc> {
        "2026-10-20T15:00:18Z".parse().unwrap()
    }

    fn press(a: &mut ReviewApp, c: char) {
        a.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    /// Moves the cursor down until it is on PR `n`.
    fn select(a: &mut ReviewApp, n: u32) {
        for _ in 0..40 {
            if a.selected_pr().is_some_and(|p| p.pr.number == n) {
                return;
            }
            press(a, 'j');
        }
        panic!("#{n} not reachable");
    }

    fn draw(a: &ReviewApp, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let screen = a.screen(now());
        let mut scroll = 0;
        terminal
            .draw(|f| {
                render(f, &screen, &mut scroll);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// One string per terminal line, skipping cells hidden behind wide characters.
    fn text_lines(buf: &Buffer) -> Vec<String> {
        let width = buf.area.width as usize;
        buf.content
            .chunks(width)
            .map(|row| {
                let mut out = String::new();
                let mut skip = 0;
                for c in row {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    out.push_str(c.symbol());
                    skip = Span::raw(c.symbol()).width().saturating_sub(1);
                }
                out.trim_end().to_string()
            })
            .collect()
    }

    fn text(buf: &Buffer) -> String {
        text_lines(buf).join("\n")
    }

    fn with_drawer(n: u32) -> ReviewApp {
        let mut a = fixture_app();
        select(&mut a, n);
        press(&mut a, ' ');
        a
    }

    #[test]
    fn review_user_120() {
        insta::assert_snapshot!(text(&draw(&fixture_app(), 120, 30)));
    }

    #[test]
    fn review_user_80() {
        insta::assert_snapshot!(text(&draw(&fixture_app(), 80, 30)));
    }

    #[test]
    fn review_user_44() {
        insta::assert_snapshot!(text(&draw(&fixture_app(), 44, 40)));
    }

    #[test]
    fn review_user_34() {
        insta::assert_snapshot!(text(&draw(&fixture_app(), 34, 40)));
    }

    #[test]
    fn review_repo_120() {
        let mut a = fixture_app();
        press(&mut a, 'g');
        insta::assert_snapshot!(text(&draw(&a, 120, 30)));
    }

    #[test]
    fn review_drawer_120_on_1312() {
        let t = text(&draw(&with_drawer(1312), 120, 40));
        assert!(t.contains("Closes CORE-2210"));
        assert!(!t.contains("sixth line"));
        assert!(t.contains("@billing") && t.contains("(code owner)"));
        assert!(!t.contains("@jayhogan"));
        insta::assert_snapshot!(t);
    }

    #[test]
    fn review_drawer_44_on_1312() {
        insta::assert_snapshot!(text(&draw(&with_drawer(1312), 44, 50)));
    }

    #[test]
    fn review_drawer_no_description_120() {
        let t = text(&draw(&with_drawer(430), 120, 40));
        assert!(t.contains("no description"));
    }

    #[test]
    fn review_folded_80() {
        let mut a = fixture_app();
        press(&mut a, ' ');
        insta::assert_snapshot!(text(&draw(&a, 80, 30)));
    }

    #[test]
    fn review_not_configured_80() {
        let a = ReviewApp::new(ReviewConfig::Missing {
            path: "/home/j/.config/herdr-pr-pulse/config.toml".into(),
        });
        insta::assert_snapshot!(text(&draw(&a, 80, 16)));
    }

    #[test]
    fn review_empty_80() {
        let mut a = fixture_app();
        let mut c = crate::review::fixture_cache().unwrap();
        c.prs.retain(|p| p.pr.number == 421 || p.pr.number == 431);
        a.set_cache(c);
        insta::assert_snapshot!(text(&draw(&a, 80, 16)));
    }

    #[test]
    fn review_help_80() {
        let mut a = fixture_app();
        press(&mut a, '?');
        insta::assert_snapshot!(text(&draw(&a, 80, 30)));
    }

    #[test]
    fn star_is_yellow_and_waiting_row_is_dim() {
        let buf = draw(&fixture_app(), 120, 30);
        let lines = text_lines(&buf);
        let w = buf.area.width as usize;
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
        let cells = |y: usize| &buf.content[y * w..(y + 1) * w];
        let star = cells(row("#1298"))
            .iter()
            .find(|c| c.symbol() == "★")
            .unwrap();
        assert_eq!(star.fg, Color::Yellow);
        let waiting = cells(row("#91"));
        let hash = waiting.iter().position(|c| c.symbol() == "#").unwrap();
        assert_eq!(waiting[hash].fg, Color::DarkGray);
        assert!(waiting.iter().any(|c| c.symbol() == "↩"));
    }

    #[test]
    fn long_title_clips_at_34() {
        let lines = text_lines(&draw(&fixture_app(), 34, 40));
        let title = lines.iter().find(|l| l.contains("#1312")).unwrap();
        assert!(title.contains('…'), "{title}");
    }

    #[test]
    fn stale_rows_are_dim_with_banner() {
        let mut a = fixture_app();
        let mut c = crate::review::fixture_cache().unwrap();
        c.error = Some(crate::github::GhError::LoginExpired);
        a.set_cache(c);
        let buf = draw(&a, 120, 30);
        let t = text(&buf);
        assert!(t.contains("GitHub login expired"));
        assert!(t.contains("offline"));
    }

    #[test]
    fn teams_notice_shows() {
        let mut a = fixture_app();
        let mut c = crate::review::fixture_cache().unwrap();
        c.teams = None;
        a.set_cache(c);
        assert!(text(&draw(&a, 120, 30)).contains("gh auth refresh -s read:org"));
    }
}
