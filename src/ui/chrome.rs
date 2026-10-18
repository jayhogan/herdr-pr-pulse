//! Header bar, summary line, column titles and key hints (boards 1-3).

use ratatui::text::{Line, Span};

use crate::ago;
use crate::app::{Body, Header, Scope, Screen, SortOrder, ViewKind};
use crate::ui::cells::{cell, dw, fit, plural};
use crate::ui::rows::Density::{self, *};
use crate::ui::rows::{title_width, widths};
use crate::ui::states::banner_lines;
use crate::ui::theme;

pub(super) fn top_lines(screen: &Screen, d: Density, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![header_bar(&screen.header, screen, d, width)];
    // The banner takes the summary's place (board 4).
    match &screen.banner {
        Some(b) => out.extend(banner_lines(screen, b, d, width)),
        None => out.push(summary(screen, d, width)),
    }
    if matches!(d, Wide | Medium) {
        out.push(Line::styled("─".repeat(width), theme::dim()));
        if !screen.help && matches!(screen.body, Body::Groups(_)) {
            out.push(column_titles(d, width));
        }
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
    let branch = workspace_branch(h).map_or(String::new(), |b| format!(" · ⎇ {b}"));
    let left = match d {
        Wide => format!(
            "│ {scope} · {counts} · {}{branch}",
            plural(h.repos, "repo", "repos")
        ),
        // Nothing listed (loading, empty): the scope says more than "0 PRs" (board 4).
        Medium if h.stacks == 0 => format!("│ {scope}{branch}"),
        Medium => format!("│ {counts}{branch}"),
        Narrow | Tiny => String::new(),
    };
    let sort = match h.sort {
        SortOrder::Urgency => "urgency",
        SortOrder::Updated => "updated",
        SortOrder::Repo => "repo",
    };
    let spin = if h.fetching { h.spinner } else { "↻" };
    let age = h.fetched_at.map(|t| ago::short(t, screen.now));
    let right = match (d, age) {
        // A failed fetch: how old the rows are, and the spinner too while retrying.
        (_, age) if h.stale => {
            let busy = if h.fetching {
                format!("{} ", h.spinner)
            } else {
                String::new()
            };
            let age = age.map_or(String::new(), |a| format!(" · stale {a}"));
            let t = format!("{busy}✗ offline{age} ");
            if d == Wide {
                format!("sort: {sort} ▾  │  {t}")
            } else {
                t
            }
        }
        // Nothing fetched yet: the spinner only while a fetch runs.
        (Wide | Medium, None) if h.fetching => format!("{} loading… ", h.spinner),
        (_, None) if h.fetching => format!("{} loading ", h.spinner),
        (Wide | Medium, None) => format!("sort: {sort} ▾ "),
        (_, None) => String::new(),
        (Wide, Some(age)) => {
            let at = h.fetched_at.unwrap().with_timezone(&screen.clock);
            format!(
                "sort: {sort} ▾  │  {spin} {} · {age} ago ",
                at.format("%H:%M:%S")
            )
        }
        (Medium, Some(age)) => format!("sort: {sort} ▾ │ {spin} {age} "),
        (_, Some(age)) => format!("{spin} {age} "),
    };
    let title = if matches!(d, Wide | Medium) {
        " PR Pulse "
    } else {
        " PR Pulse"
    };
    let room = width.saturating_sub(dw(title) + dw(&right));
    let left = fit(&left, room);
    let gap = room.saturating_sub(dw(&left));
    let spans = vec![
        Span::styled(fit(title, width), theme::bold()),
        Span::raw(format!("{left}{}{right}", " ".repeat(gap))),
    ];
    Line::from(spans).style(theme::bar())
}

fn summary(screen: &Screen, d: Density, width: usize) -> Line<'static> {
    let c = &screen.summary.counts;
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
    // ✦ what changed since you looked (board 6 A): right-aligned when wide, after the counts
    // when narrow; nothing when nothing changed.
    let n = screen.summary.changed;
    if n > 0 {
        let used: usize = spans.iter().map(Span::width).sum();
        match d {
            Wide | Medium => {
                let since = screen.summary.since.map_or(String::new(), |t| {
                    format!(" {} ago", ago::short(t, screen.now))
                });
                // The long form when it fits beside the counts, else shorter.
                let t = [
                    format!("✦ {n} changed since you looked{since} "),
                    format!("✦ {n} changed "),
                    format!("✦{n} new "),
                ]
                .into_iter()
                .find(|t| used + 1 + dw(t) <= width)
                .unwrap_or_default();
                spans.push(Span::raw(" ".repeat(width.saturating_sub(used + dw(&t)))));
                spans.push(Span::styled(t, theme::bold()));
            }
            Narrow | Tiny => {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(format!("✦{n} new"), theme::bold()));
            }
        }
    }
    // Narrow Workspace view: `⎇ branch` right-aligned (board 2); wider ones put it in the bar.
    if let (Some(b), Narrow | Tiny) = (workspace_branch(&screen.header), d) {
        let used: usize = spans.iter().map(Span::width).sum();
        let b = if d == Tiny { fit(&b, 12) } else { b };
        let room = width.saturating_sub(used + 1);
        let t = fit(&format!("⎇ {b} "), room);
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + dw(&t)))));
        spans.push(Span::styled(t, theme::dim()));
    }
    Line::from(spans)
}

/// The header `⎇` branch: Workspace view only.
fn workspace_branch(h: &Header) -> Option<String> {
    h.branch.clone().filter(|_| h.view == ViewKind::Workspace)
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
    let help_keys = [
        (" j/k", " scroll  "),
        ("? or Esc", " close  "),
        ("q", " quit"),
    ];
    let spans: Vec<Span> = if screen.help {
        help_keys
            .into_iter()
            .flat_map(|(k, what)| {
                [
                    Span::styled(k.to_string(), theme::dim().patch(theme::bold())),
                    Span::styled(what.to_string(), theme::dim()),
                ]
            })
            .collect()
    } else {
        spans
    };
    let hints = match screen.status.as_ref().filter(|_| !screen.help) {
        Some(status) => Line::raw(fit(&format!(" {status}"), width)),
        None => Line::from(spans),
    };
    let mut lines = Vec::new();
    if let Some(f) = screen.filter.as_ref().filter(|_| !screen.help) {
        lines.push(if f.editing {
            Line::raw(fit(&format!("/ {}▏", f.text), width))
        } else {
            Line::styled(fit(&format!("/ {}", f.text), width), theme::dim())
        });
    }
    match d {
        Wide | Medium => lines.extend([Line::styled("─".repeat(width), theme::dim()), hints]),
        Narrow | Tiny => lines.push(hints),
    }
    lines
}
