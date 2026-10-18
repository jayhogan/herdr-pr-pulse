//! Header bar, summary line, column titles and key hints (boards 1-3).

use ratatui::text::{Line, Span};

use crate::ago;
use crate::app::{Header, Scope, Screen, SortOrder, Summary, ViewKind};
use crate::ui::cells::{cell, dw, fit, plural};
use crate::ui::rows::Density::{self, *};
use crate::ui::rows::{title_width, widths};
use crate::ui::theme;

pub(super) fn top_lines(screen: &Screen, d: Density, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![header_bar(&screen.header, screen, d, width)];
    out.push(summary(&screen.summary, d));
    if matches!(d, Wide | Medium) {
        out.push(Line::styled("─".repeat(width), theme::dim()));
        out.push(column_titles(d, width));
    }
    out
}

fn header_bar(h: &Header, screen: &Screen, d: Density, width: usize) -> Line<'static> {
    let scope = match (h.view, h.scope) {
        (ViewKind::Workspace, _) => "workspace",
        (ViewKind::Mine, Scope::Both) => "authored + assigned",
        (ViewKind::Mine, Scope::Authored) => "authored",
        (ViewKind::Mine, Scope::Assigned) => "assigned",
    };
    let counts = format!(
        "{} · {}",
        plural(h.prs, "PR", "PRs"),
        plural(h.stacks, "stack", "stacks")
    );
    let left = match d {
        Wide => format!(
            "│ {scope} · {counts} · {}",
            plural(h.repos, "repo", "repos")
        ),
        Medium => format!("│ {counts}"),
        Narrow | Tiny => String::new(),
    };
    let sort = match h.sort {
        SortOrder::Urgency => "urgency",
        SortOrder::Updated => "updated",
        SortOrder::Repo => "repo",
    };
    let spin = if h.fetching { "⠹" } else { "↻" };
    let age = h.fetched_at.map(|t| ago::short(t, screen.now));
    let right = match (d, age) {
        // Minimal stale marker; Task 18 draws the full error states.
        (Wide | Medium, Some(age)) if h.stale => format!("sort: {sort} ▾ │ ✗ stale {age} "),
        (_, Some(age)) if h.stale => format!("✗ stale {age} "),
        (Wide | Medium, None) if h.stale => format!("sort: {sort} ▾ │ ✗ stale "),
        (_, None) if h.stale => "✗ stale ".into(),
        (Wide, Some(age)) => {
            let at = h.fetched_at.unwrap().with_timezone(&screen.clock);
            format!(
                "sort: {sort} ▾  │  {spin} {} · {age} ago ",
                at.format("%H:%M:%S")
            )
        }
        (Wide, None) => format!("sort: {sort} ▾ "),
        (Medium, Some(age)) => format!("sort: {sort} ▾ │ {spin} {age} "),
        (Medium, None) => format!("sort: {sort} ▾ "),
        (_, Some(age)) => format!("{spin} {age} "),
        (_, None) => String::new(),
    };
    let title = if matches!(d, Wide | Medium) {
        " PR Pulse "
    } else {
        " PR Pulse"
    };
    let room = width.saturating_sub(dw(title) + dw(&right));
    let left = fit(&left, room);
    let gap = room - dw(&left);
    let spans = vec![
        Span::styled(fit(title, width), theme::bold()),
        Span::raw(format!("{left}{}{right}", " ".repeat(gap))),
    ];
    Line::from(spans).style(theme::bar())
}

fn summary(s: &Summary, d: Density) -> Line<'static> {
    let c = &s.counts;
    let parts = [
        (
            c.urgent,
            "▲▲",
            "urgent",
            "urgent",
            theme::red().patch(theme::bold()),
        ),
        (
            c.needs_you,
            "▲",
            "need you",
            "need you",
            theme::red().patch(theme::bold()),
        ),
        (
            c.ready,
            "◆",
            "ready to merge",
            "merge",
            theme::green().patch(theme::bold()),
        ),
        (c.nudge, "△", "nudge", "nudge", theme::yellow()),
        (c.waiting, "◷", "waiting", "waiting", theme::dim()),
    ];
    let mut spans = vec![Span::raw(" ")];
    let sep = match d {
        Wide => "    ",
        Medium => "  ",
        Narrow | Tiny => " ",
    };
    for (n, glyph, long, medium, style) in parts.into_iter().filter(|p| p.0 > 0) {
        if spans.len() > 1 {
            spans.push(Span::raw(sep));
        }
        let t = match d {
            Wide => format!("{glyph} {n} {long}"),
            Medium => format!("{glyph}{n} {medium}"),
            Narrow | Tiny => format!("{glyph}{n}"),
        };
        spans.push(Span::styled(t, style));
    }
    Line::from(spans)
}

fn column_titles(d: Density, width: usize) -> Line<'static> {
    let names = match d {
        Wide => [
            "", "!", "LAYER", "#", "STATE", "CI", "THRD", "REVIEW", "MERGE",
        ],
        _ => ["", "!", "LYR", "#", "", "CI", "TH", "REV", "MERGE"],
    };
    let w = widths(d);
    let mut spans: Vec<Span> = (0..5).map(|i| cell(names[i], w[i], theme::dim())).collect();
    spans.push(cell("TITLE", title_width(d, width), theme::dim()));
    spans.extend((5..9).map(|i| cell(names[i], w[i], theme::dim())));
    Line::from(spans)
}

pub(super) fn footer(screen: &Screen, d: Density, width: usize) -> Vec<Line<'static>> {
    let mine = screen.header.view == ViewKind::Mine;
    let keys: Vec<(&str, &str)> = match d {
        Wide => {
            let mut k = vec![
                (" j/k", " move  "),
                ("␣", " details  "),
                ("⏎", " jump  "),
                ("o", " open  "),
                ("r", " refresh  "),
            ];
            if mine {
                k.push(("s", " authored⇄assigned  "));
            }
            k.extend([
                ("S", " sort  "),
                ("h/l", " fold  "),
                ("/", " filter  "),
                ("?", " help"),
            ]);
            k
        }
        Medium => {
            let mut k = vec![
                (" j/k", ""),
                (" ␣", " info "),
                ("⏎", " jump "),
                ("o", " open "),
                ("r", " ↻ "),
            ];
            if mine {
                k.push(("s", " mine⇄asgn "));
            }
            k.extend([("S", " sort "), ("h/l", " fold "), ("?", " help")]);
            k
        }
        Narrow => vec![
            ("j/k", " move "),
            ("␣", " details "),
            ("⏎", " jump "),
            ("o", " open "),
            ("?", " keys"),
        ],
        Tiny => vec![
            ("j/k", ""),
            (" ␣", " info"),
            (" ⏎", " jump"),
            (" ?", " keys"),
        ],
    };
    let spans: Vec<Span> = keys
        .into_iter()
        .flat_map(|(k, what)| {
            [
                Span::styled(k.to_string(), theme::dim().patch(theme::bold())),
                Span::styled(what.to_string(), theme::dim()),
            ]
        })
        .collect();
    let hints = Line::from(spans);
    match d {
        Wide | Medium => vec![Line::styled("─".repeat(width), theme::dim()), hints],
        Narrow | Tiny => vec![hints],
    }
}
