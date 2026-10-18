//! Turns the `Screen` view model into terminal lines (spec §10, boards 1-3).
//!
//! Wide/Medium draw one line per PR (boards 1 and 3); Narrow/Tiny draw two (board 2, with the
//! 34-column words below 40). All widths are display widths, never byte lengths.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ago;
use crate::app::{Body, GroupView, Header, RowView, Scope, Screen, SortOrder, Summary, ViewKind};
use crate::attention::{Attention, Reason, reasons};
use crate::model::{CiState, MergeState, Pr, PrState, ReviewState};
use crate::stacks::GroupKind;
use crate::ui::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Wide,
    Medium,
    Narrow,
    Tiny,
}

use Density::*;

pub fn density(width: u16) -> Density {
    match width {
        100.. => Wide,
        60..=99 => Medium,
        40..=59 => Narrow,
        _ => Tiny,
    }
}

/// One word per density: [Wide, Medium, Narrow, Tiny].
fn pick(d: Density, words: [&str; 4]) -> String {
    let i = match d {
        Wide => 0,
        Medium => 1,
        Narrow => 2,
        Tiny => 3,
    };
    words[i].to_string()
}

fn dw(s: &str) -> usize {
    Span::raw(s).width()
}

/// `s` cut to at most `width` display columns, ending in `…` if cut.
pub fn fit(s: &str, width: usize) -> String {
    if dw(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = dw(c.encode_utf8(&mut [0; 4]));
        if used + cw > width - 1 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

/// A table cell: `text` fitted to `width - 1` columns and padded to exactly `width`.
fn cell(text: &str, width: usize, style: Style) -> Span<'static> {
    let t = fit(text, width.saturating_sub(1));
    let pad = width.saturating_sub(dw(&t));
    Span::styled(format!("{t}{}", " ".repeat(pad)), style)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Repo name without the owner.
fn short_repo(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

// ---------------------------------------------------------------- layout and scrolling

/// Draws the screen: header bar and summary on top, key hints at the bottom, and the
/// groups in between, scrolled so the cursor stays visible.
pub fn render(frame: &mut Frame, screen: &Screen, scroll: &mut usize) {
    let area = frame.area();
    let d = density(area.width);
    let width = area.width as usize;
    let top = top_lines(screen, d, width);
    let bottom = footer(screen, d, width);
    let (body, cursor) = lines(screen, area.width);
    let cursor_end = cursor + if matches!(d, Narrow | Tiny) { 1 } else { 0 };

    let top_h = (top.len() as u16).min(area.height);
    let bottom_h = (bottom.len() as u16).min(area.height - top_h);
    let body_h = area.height - top_h - bottom_h;
    let h = body_h as usize;
    if h > 0 {
        if cursor < *scroll {
            *scroll = cursor;
        } else if cursor_end >= *scroll + h {
            *scroll = (cursor_end + 1).saturating_sub(h).min(cursor);
        }
    }
    *scroll = (*scroll).min(body.len().saturating_sub(h));

    let rect = |y: u16, h: u16| Rect::new(area.x, area.y + y, area.width, h);
    frame.render_widget(Paragraph::new(top), rect(0, top_h));
    let scrolled = u16::try_from(*scroll).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(body).scroll((scrolled, 0)),
        rect(top_h, body_h),
    );
    frame.render_widget(Paragraph::new(bottom), rect(top_h + body_h, bottom_h));
}

/// The scrollable body lines and the index of the cursor's (first) line.
pub fn lines(screen: &Screen, width: u16) -> (Vec<Line<'static>>, usize) {
    let d = density(width);
    let width = width as usize;
    let mut out = Vec::new();
    let mut cursor_line = 0;
    let groups = match &screen.body {
        Body::Loading(_) => {
            out.push(Line::styled(fit("  ⠹ loading…", width), theme::dim()));
            return (out, 0);
        }
        Body::Empty(_) => {
            out.push(Line::styled(fit("  no open PRs", width), theme::dim()));
            return (out, 0);
        }
        Body::Groups(gs) => gs,
    };
    let mut selectable = 0;
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            out.push(Line::raw(""));
        }
        if selectable == screen.cursor {
            cursor_line = out.len();
        }
        out.push(group_header(g, d, selectable == screen.cursor));
        selectable += 1;
        if g.folded {
            continue;
        }
        if g.kind != GroupKind::Standalone {
            let base = match d {
                Wide => format!("     ╭─ {}", g.base),
                Medium => format!("   ╭─ {}", g.base),
                Narrow | Tiny => format!(" ╭ {}", g.base),
            };
            out.push(Line::styled(base, theme::dim()));
        }
        for r in &g.rows {
            let cur = selectable == screen.cursor;
            if cur {
                cursor_line = out.len();
            }
            out.extend(row_lines(g, r, d, width, cur));
            selectable += 1;
        }
    }
    (out, cursor_line)
}

// ---------------------------------------------------------------- header, summary, footer

fn top_lines(screen: &Screen, d: Density, width: usize) -> Vec<Line<'static>> {
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

/// Column widths for one-line rows: cursor, attention, layer, #, state, CI, threads,
/// review, merge. The title takes what is left.
fn widths(d: Density) -> [usize; 9] {
    match d {
        Wide => [2, 3, 6, 7, 8, 21, 6, 15, 21],
        _ => [2, 2, 4, 6, 2, 14, 4, 6, 18],
    }
}

fn title_width(d: Density, width: usize) -> usize {
    width.saturating_sub(widths(d).iter().sum())
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

fn footer(screen: &Screen, d: Density, width: usize) -> Vec<Line<'static>> {
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

// ---------------------------------------------------------------- group headers

/// Reasons in the same severity class as the first one (needs you / nudge / waiting).
fn first_class(reasons: &[Reason]) -> Vec<&Reason> {
    let class = |r: &Reason| match r {
        Reason::CiFailing(_) | Reason::ChangesRequested | Reason::Conflicts => 0,
        Reason::Behind(_) | Reason::Threads(_) | Reason::NoReviewers => 1,
        _ => 2,
    };
    let Some(first) = reasons.first() else {
        return Vec::new();
    };
    reasons
        .iter()
        .filter(|r| class(r) == class(first))
        .collect()
}

fn reason_words(r: &Reason, base: &str, d: Density) -> String {
    let wide = d == Wide;
    match r {
        Reason::CiFailing(_) if wide => "CI failing".into(),
        Reason::CiFailing(_) => "CI ✗".into(),
        Reason::ChangesRequested if wide => "changes requested".into(),
        Reason::ChangesRequested => "✗ chg req".into(),
        Reason::Conflicts if wide => format!("conflicts with {base}"),
        Reason::Conflicts => "⚠ conflicts".into(),
        Reason::Behind(n) if wide => format!("behind {base} by {n}"),
        Reason::Behind(n) => format!("↓ behind {n}"),
        Reason::Threads(n) if wide => plural(*n as usize, "open thread", "open threads"),
        Reason::Threads(n) => format!("◇{n} threads"),
        Reason::NoReviewers if wide => "no reviewers".into(),
        Reason::NoReviewers => "∅ reviewers".into(),
        Reason::CiRunning if wide => "CI running".into(),
        Reason::CiRunning => "CI ◐".into(),
        Reason::ReviewsPending if wide => "reviews pending".into(),
        Reason::ReviewsPending => "○ reviews".into(),
        Reason::Computing if wide => "merge state computing".into(),
        Reason::Computing => "? computing".into(),
        Reason::Draft if wide => "draft".into(),
        Reason::Draft => "◌ draft".into(),
    }
}

/// Style for a reason, by the level it would cause.
fn reason_style(r: &Reason) -> Style {
    match r {
        Reason::CiFailing(_) | Reason::ChangesRequested | Reason::Conflicts => theme::red(),
        Reason::Behind(_) | Reason::Threads(_) | Reason::NoReviewers => theme::yellow(),
        _ => theme::dim(),
    }
}

fn minimap(g: &GroupView) -> Vec<Span<'static>> {
    g.minimap
        .iter()
        .map(|&a| match a {
            // The red-background ▲▲ belongs to the row; the minimap keeps one cell per layer.
            Attention::Urgent => Span::styled("▲", theme::attention(Attention::NeedsYou).1),
            a => {
                let (glyph, style) = theme::attention(a);
                Span::styled(glyph, style)
            }
        })
        .collect()
}

/// What the stack header says about merging, e.g. "mergeable through layer 1 of 4 · …".
fn merge_story(g: &GroupView, d: Density) -> Vec<Span<'static>> {
    let open = g.open_layers;
    let m = g.mergeable_through;
    let standalone = g.kind == GroupKind::Standalone;
    let mut spans = Vec::new();
    if standalone {
        let Some(r) = g.rows.iter().find(|r| r.attention.is_some()) else {
            return spans;
        };
        if r.attention == Some(Attention::Ready) {
            let t = pick(d, ["mergeable now", "merge now", "", ""]);
            spans.push(Span::styled(t, theme::green()));
            return spans;
        }
        let rs = reasons(&r.pr);
        let class = first_class(&rs);
        let (Some(first), base) = (class.first(), &r.pr.base_ref) else {
            return spans;
        };
        let t = match (first, d) {
            (Reason::Behind(n), Wide) => {
                format!("mergeable after update · behind {base} by {n}")
            }
            (Reason::Behind(n), _) => format!("update needed · behind {base} {n}"),
            (_, Wide) => class
                .iter()
                .map(|r| reason_words(r, base, d))
                .collect::<Vec<_>>()
                .join(", "),
            (r, _) => reason_words(r, base, d),
        };
        spans.push(Span::styled(t, reason_style(first)));
        return spans;
    }
    match d {
        Wide => {
            spans.push(Span::raw("mergeable through "));
            spans.push(Span::styled(format!("layer {m} of {open}"), theme::bold()));
        }
        Medium => {
            spans.push(Span::raw("ok through "));
            spans.push(Span::styled(format!("{m}/{open}"), theme::bold()));
        }
        Narrow => spans.push(Span::raw(format!("through L{m} of {open}"))),
        Tiny => spans.push(Span::raw(format!("L{m}/{open} ok"))),
    }
    if let (Some((layer, first)), Wide | Medium) = (&g.blocker, d) {
        let row = g.rows.iter().find(|r| r.layer == *layer);
        let all = row.map(|r| reasons(&r.pr)).unwrap_or_default();
        let words = if d == Wide {
            first_class(&all)
                .iter()
                .map(|r| reason_words(r, &g.base, d))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            reason_words(first, &g.base, d)
        };
        let t = match (d, layer) {
            (Wide, 1) => format!(" · L1 {words}"),
            (Wide, _) => format!(" · stuck at L{layer}: {words}"),
            _ => format!(" · L{layer} {words}"),
        };
        spans.push(Span::styled(t, reason_style(first)));
    }
    spans
}

fn group_header(g: &GroupView, d: Density, cursor: bool) -> Line<'static> {
    let fold = if g.folded { "▸ " } else { "▾ " };
    let mut spans = Vec::new();
    if cursor {
        spans.push(Span::styled("▶", theme::bold()));
    }
    spans.push(Span::styled(fold, theme::bold()));
    let standalone = g.kind == GroupKind::Standalone;
    let name_style = if standalone {
        theme::dim()
    } else {
        theme::cyan().patch(theme::bold())
    };
    let map = minimap(g);
    let map_w: usize = map.iter().map(Span::width).sum();
    match d {
        Wide | Medium => {
            let repo = if d == Wide {
                g.repo.clone()
            } else {
                short_repo(&g.repo).to_string()
            };
            spans.push(Span::styled(repo, theme::bold()));
            let dot = if d == Wide { " · " } else { "·" };
            spans.push(Span::styled(dot, theme::dim()));
            spans.push(Span::styled(g.name.clone(), name_style));
            if d == Wide {
                let label = if standalone {
                    plural(g.rows.len(), "PR", "PRs")
                } else {
                    plural(g.rows.len(), "layer", "layers")
                };
                spans.push(cell(&format!("  {label}"), 12, theme::dim()));
            } else {
                spans.push(Span::raw(" "));
            }
            spans.extend(map);
            let gap = if d == Wide {
                map_w.max(4) - map_w + 3
            } else {
                2
            };
            spans.push(Span::raw(" ".repeat(gap)));
        }
        Narrow | Tiny => {
            spans.push(Span::styled(format!("{} ", g.name), name_style));
            spans.extend(map);
            spans.push(Span::raw(if d == Narrow { "  " } else { " " }));
        }
    }
    if !(standalone && matches!(d, Narrow | Tiny)) {
        spans.extend(merge_story(g, d));
    }
    if g.folded {
        spans.push(Span::styled(
            format!("  ({} hidden)", g.rows.len()),
            theme::dim(),
        ));
    }
    Line::from(spans)
}

// ---------------------------------------------------------------- PR rows

type Text = (String, Style);

fn ci(pr: &Pr, d: Density) -> Text {
    let c = &pr.ci;
    let frac = format!("{}/{}", c.passed, c.counted);
    match c.state {
        CiState::Passing => {
            let mut t = pick(d, ["✓ pass ", "✓ ", "✓ ci ", "✓ "]) + &frac;
            if d == Wide && !c.optional_failing.is_empty() {
                t += &format!(" ·{} opt ✗", c.optional_failing.len());
            }
            (t, theme::green())
        }
        CiState::Failing => {
            let name = c.failing.first().map_or("", |x| x.name.as_str());
            let fails = format!("{}/{}", c.failing.len(), c.counted);
            let t = match d {
                Wide | Medium => format!("✗ {name}"),
                Narrow => format!("✗ ci {fails} fail"),
                Tiny => format!("✗ {fails}"),
            };
            (t, theme::red().patch(theme::bold()))
        }
        CiState::Running => (
            pick(d, ["◐ running ", "◐ ", "◐ ci ", "◐ "]) + &frac,
            theme::cyan(),
        ),
        CiState::None => (pick(d, ["– none", "–", "– no ci", "– ci"]), theme::dim()),
    }
}

fn threads(pr: &Pr, d: Density) -> Text {
    match pr.unresolved_threads {
        0 => (pick(d, ["· 0", "·", "·0", "·0"]), theme::dim()),
        n if d == Wide => (format!("◇ {n}"), theme::yellow()),
        n => (format!("◇{n}"), theme::yellow()),
    }
}

fn review(pr: &Pr, d: Density) -> Text {
    let pending = pr
        .reviewers
        .iter()
        .filter(|r| r.state != ReviewState::Approved)
        .count();
    if pr.changes_requested {
        (
            pick(d, ["✗ changes req", "✗ chg", "✗ chg", "✗"]),
            theme::red(),
        )
    } else if pr.reviewers.is_empty() {
        let style = if pr.is_draft {
            theme::dim()
        } else {
            theme::yellow()
        };
        (pick(d, ["∅ no reviewers", "∅", "∅ none", "∅"]), style)
    } else if pr.approved == Some(true) || pending == 0 {
        (pick(d, ["✓ approved", "✓ ok", "✓ ok", "✓"]), theme::green())
    } else {
        let t = match d {
            Wide => format!("○ pending {pending}"),
            Medium | Narrow => format!("○ {pending}"),
            Tiny => format!("○{pending}"),
        };
        (t, theme::dim())
    }
}

fn merge(r: &RowView, d: Density) -> Text {
    let pr = &r.pr;
    match pr.state {
        PrState::Merged => return ("⊙ merged".into(), theme::dim()),
        PrState::Closed => return ("✕ closed".into(), theme::dim()),
        PrState::Open => {}
    }
    if let Some(n) = r.waiting_on {
        let t = pick(d, ["◷ waiting on #", "◷ on #", "◷ on #", "◷#"]) + &n.to_string();
        return (t, theme::dim());
    }
    if r.attention == Some(Attention::Ready) {
        let t = pick(
            d,
            ["✓ clean · merge now", "✓ merge now", "✓ clean", "✓ clean"],
        );
        return (t, theme::green().patch(theme::bold()));
    }
    let behind = |n: u32| -> Text {
        let t = match d {
            Wide => format!("↓ behind {} {n}", pr.base_ref),
            Medium | Narrow => format!("↓ behind {n}"),
            Tiny => format!("↓ {n}"),
        };
        (t, theme::yellow())
    };
    match pr.merge {
        MergeState::Clean | MergeState::HasHooks | MergeState::Unstable => {
            ("✓ clean".into(), theme::green())
        }
        MergeState::Blocked => (
            pick(d, ["⊘ blocked", "⊘ blocked", "⊘ blocked", "⊘ blkd"]),
            theme::yellow(),
        ),
        MergeState::Behind => behind(pr.behind_by.unwrap_or(0)),
        MergeState::Dirty => (
            pick(
                d,
                ["⚠ conflicts", "⚠ conflicts", "⚠ conflicts", "⚠ conflict"],
            ),
            theme::red().patch(theme::bold()),
        ),
        MergeState::Unknown => (
            pick(d, ["? computing", "? computing", "? computing", "? …"]),
            theme::dim(),
        ),
        MergeState::Draft => match pr.behind_by {
            Some(n) if n > 0 => behind(n),
            _ => (
                pick(d, ["◌ draft", "◌ draft", "◌ draft", "◌"]),
                theme::dim(),
            ),
        },
    }
}

fn tree(g: &GroupView, r: &RowView, d: Density) -> String {
    let layer = if r.layer == 0 {
        "·".to_string()
    } else {
        r.layer.to_string()
    };
    let (corner, sep) = match (g.kind, r.is_last) {
        (GroupKind::Standalone, _) => ("─", "─"),
        (_, true) => ("└", "─"),
        (_, false) => ("├", "─"),
    };
    match (g.kind, d) {
        (GroupKind::Standalone, Wide) => format!("── {layer}"),
        (GroupKind::Standalone, _) => "──".into(),
        (_, Wide) => format!("{corner}{sep} {layer}"),
        _ => format!("{corner}{layer}"),
    }
}

/// Styles dropped and the line padded to `width` on the red ▲▲ row, so every cell is red.
fn finish(spans: Vec<Span<'static>>, hot: bool, width: usize) -> Line<'static> {
    if !hot {
        return Line::from(spans);
    }
    let mut plain: Vec<Span> = spans
        .into_iter()
        .map(|s| Span::raw(s.content.into_owned()))
        .collect();
    let used: usize = plain.iter().map(Span::width).sum();
    plain.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    Line::from(plain).style(theme::hot_row())
}

fn row_lines(
    g: &GroupView,
    r: &RowView,
    d: Density,
    width: usize,
    cur: bool,
) -> Vec<Line<'static>> {
    let pr = &r.pr;
    let hot = r.attention == Some(Attention::Urgent);
    let (glyph, att_style) = r.attention.map_or(("", Style::new()), theme::attention);
    let em = if cur { theme::bold() } else { Style::new() };
    let pointer = if cur { "▶" } else { " " };
    let num = format!("#{}", pr.number);
    let draft_dim = if pr.is_draft {
        theme::dim()
    } else {
        Style::new()
    };
    let (c, t, rv, m) = (ci(pr, d), threads(pr, d), review(pr, d), merge(r, d));
    match d {
        Wide | Medium => {
            let w = widths(d);
            let mut spans = Vec::new();
            if d == Medium && dw(glyph) > 1 {
                // ▲▲ is two cells wide: borrow the cursor column's spacer to keep alignment.
                spans.push(Span::styled(pointer, theme::bold()));
                spans.push(Span::styled(format!("{glyph} "), att_style));
            } else {
                spans.push(cell(pointer, w[0], theme::bold()));
                spans.push(cell(glyph, w[1], att_style));
            }
            let state = if pr.is_draft {
                pick(d, ["◌ draft", "◌", "", ""])
            } else {
                pick(d, ["● ready", "●", "", ""])
            };
            spans.extend([
                cell(&tree(g, r, d), w[2], theme::dim()),
                cell(&num, w[3], em),
                cell(&state, w[4], draft_dim),
                cell(&pr.title, title_width(d, width), em),
                cell(&c.0, w[5], c.1),
                cell(&t.0, w[6], t.1),
                cell(&rv.0, w[7], rv.1),
                cell(&m.0, w[8], m.1),
            ]);
            vec![finish(spans, hot, width)]
        }
        Narrow | Tiny => {
            let title_w = width.saturating_sub(4 + 3 + 6 + 2);
            let line1 = vec![
                Span::styled(pointer, theme::bold()),
                cell(&tree(g, r, d), 3, theme::dim()),
                cell(glyph, 3, att_style),
                cell(&num, 6, theme::bold()),
                cell(&pr.title, title_w, draft_dim.patch(em)),
            ];
            let (ci_w, rv_w, th_w) = if d == Narrow { (14, 7, 5) } else { (10, 4, 5) };
            let mg_w = width.saturating_sub(4 + ci_w + rv_w + th_w);
            let rail = if r.is_last || g.kind == GroupKind::Standalone {
                "  "
            } else {
                " │"
            };
            let line2 = vec![
                cell(rail, 4, theme::dim()),
                cell(&c.0, ci_w, c.1),
                cell(&rv.0, rv_w, rv.1),
                cell(&t.0, th_w, t.1),
                cell(&m.0, mg_w, m.1),
            ];
            vec![finish(line1, hot, width), finish(line2, hot, width)]
        }
    }
}
