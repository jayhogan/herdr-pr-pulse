//! The row drawer opened with ␣ (boards 1-3): CI, Reviews, Merge (and Changed) for one PR.
//!
//! From 80 columns the sections sit side by side (board 1 at Wide, board 3 at Medium);
//! below that they stack, one under the other (board 2).

use chrono::{DateTime, Utc};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ago;
use crate::app::RowView;
use crate::model::{MergeState, ReviewState, Reviewer};
use crate::ui::cells::{dw, fit};
use crate::ui::theme;

/// How the drawer is laid out; decides columns and how many words each line gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wide,
    Medium,
    Narrow,
    Tiny,
}

impl Mode {
    fn of(width: u16) -> Mode {
        match width {
            120.. => Mode::Wide,
            80..=119 => Mode::Medium,
            40..=79 => Mode::Narrow,
            _ => Mode::Tiny,
        }
    }

    fn side_by_side(self) -> bool {
        matches!(self, Mode::Wide | Mode::Medium)
    }

    /// Full words (`changes`, `up to date with #1284`) rather than the short ones.
    fn full(self) -> bool {
        matches!(self, Mode::Wide | Mode::Narrow)
    }
}

/// One line of a section: the left part, and an optional part after it (right-aligned when
/// stacked, after `gap` when side by side).
struct Item {
    left: Vec<Span<'static>>,
    gap: &'static str,
    right: Option<Span<'static>>,
    /// Side by side: shown under the columns across their full width, not clipped to one.
    full_width: bool,
}

impl Item {
    fn new(left: Vec<Span<'static>>) -> Item {
        Item {
            left,
            gap: "",
            right: None,
            full_width: false,
        }
    }

    fn heading(text: &str) -> Item {
        Item::new(vec![Span::styled(
            text.to_string(),
            theme::dim().patch(theme::bold()),
        )])
    }
}

struct Section {
    title: &'static str,
    items: Vec<Item>,
}

fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

fn padded(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(dw(text))))
}

// ---------------------------------------------------------------- sections

fn ci_section(row: &RowView, mode: Mode, now: DateTime<Utc>) -> Section {
    let ci = &row.pr.ci;
    let mut items = Vec::new();
    for check in &ci.failing {
        let mut item = Item::new(vec![
            span("✗ ", theme::red().patch(theme::bold())),
            span(check.name.clone(), theme::bold()),
        ]);
        // Board 3 leaves the age out.
        let age = check.completed_at.map(|t| match mode {
            Mode::Tiny => ago::short(t, now),
            _ => format!("{} ago", ago::short(t, now)),
        });
        if let (Some(age), true) = (age, mode != Mode::Medium) {
            item.gap = "   ";
            item.right = Some(span(age, theme::dim()));
        }
        items.push(item);
    }
    if ci.running > 0 {
        items.push(Item::new(vec![span(
            format!("◐ {} running", ci.running),
            theme::cyan(),
        )]));
    }
    if ci.passed > 0 {
        let text = match (ci.failing.is_empty(), mode.full()) {
            (true, true) => format!("{} checks pass", ci.passed),
            (true, false) => format!("{} pass", ci.passed),
            (false, true) => format!("{} other checks pass", ci.passed),
            (false, false) => format!("{} others pass", ci.passed),
        };
        items.push(Item::new(vec![
            span("✓ ", theme::green()),
            span(text, theme::dim()),
        ]));
    }
    if ci.counted == 0 && items.is_empty() {
        items.push(Item::new(vec![span("– no checks", theme::dim())]));
    }
    if !ci.optional_failing.is_empty() {
        items.push(Item::heading("Optional"));
        for check in &ci.optional_failing {
            items.push(Item::new(vec![
                span("✗ ", theme::yellow()),
                span(check.name.clone(), theme::dim()),
            ]));
        }
    }
    Section { title: "CI", items }
}

fn review_item(r: &Reviewer, mode: Mode, login_min: usize) -> Item {
    let (glyph, style, words) = match r.state {
        ReviewState::Approved => ("✓", theme::green(), ["approved", "ok"]),
        ReviewState::ChangesRequested => ("✗", theme::red(), ["changes", "chg"]),
        ReviewState::Commented => ("·", theme::dim(), ["commented", "cmt"]),
        ReviewState::Pending => ("○", theme::dim(), ["pending", "pend"]),
    };
    let word = words[usize::from(!mode.full())];
    let (base_w, state_w) = match mode {
        Mode::Wide => (9, 10),
        Mode::Medium => (7, 5),
        _ => (if mode.full() { 9 } else { 7 }, 0),
    };
    // Never touching the state word, whatever the login.
    let mut login_w = base_w.max(login_min);
    let mut login = format!("@{}", r.login);
    if mode == Mode::Medium {
        // The column is 24 wide: keep room for the state word and `◇n open`.
        login_w = login_w.min(8);
        login = fit(&login, login_w - 1);
    }
    let mut item = Item::new(vec![
        span(format!("{glyph} "), style),
        span(padded(&login, login_w), Style::new()),
        span(padded(word, state_w), style),
    ]);
    if r.unresolved_threads > 0 {
        item.right = Some(span(
            format!("◇{} open", r.unresolved_threads),
            theme::yellow(),
        ));
    }
    item
}

fn reviews_section(row: &RowView, mode: Mode) -> Section {
    let (bots, people): (Vec<&Reviewer>, Vec<&Reviewer>) =
        row.pr.reviewers.iter().partition(|r| r.is_bot);
    let mut items = Vec::new();
    if people.is_empty() && bots.is_empty() {
        items.push(Item::new(vec![span("∅ no reviewers", theme::dim())]));
    }
    // Headings only help when both kinds are there (board 1 shows people alone without one).
    let headed = !people.is_empty() && !bots.is_empty();
    for (name, group) in [("People", people), ("Bots", bots)] {
        if headed {
            items.push(Item::heading(name));
        }
        // Logins line up within a group, not across People and Bots.
        let login_min = group.iter().map(|r| dw(&r.login) + 3).max().unwrap_or(0);
        items.extend(group.into_iter().map(|r| review_item(r, mode, login_min)));
    }
    Section {
        title: "Reviews",
        items,
    }
}

fn merge_section(row: &RowView, mode: Mode) -> Section {
    let pr = &row.pr;
    let base = &pr.base_ref;
    let up_to_date = |what: String| {
        let text = match mode {
            Mode::Medium => format!("current w/ {what}"),
            Mode::Wide | Mode::Narrow => format!("up to date with {what}"),
            Mode::Tiny => "up to date".to_string(),
        };
        ("✓", text, theme::green(), Style::new())
    };
    let behind = match pr.behind_by {
        Some(n) => format!("behind {base} by {n}"),
        None => format!("behind {base}"),
    };
    let (glyph, text, glyph_style, text_style) = match (pr.merge, row.waiting_on) {
        (MergeState::Blocked, None) if row.stack_ready.is_some() => {
            let (k, n) = row.stack_ready.unwrap_or_default();
            (
                "✓",
                format!("ready to merge as a stack ({k} of {n})"),
                theme::green(),
                theme::green(),
            )
        }
        (MergeState::Behind, _) => ("↓", behind, theme::yellow(), theme::yellow()),
        (MergeState::Dirty, _) => (
            "⚠",
            format!("conflicts with {base}"),
            theme::red(),
            theme::red(),
        ),
        (_, Some(below)) => up_to_date(format!("#{below}")),
        (MergeState::Unknown, None) => (
            "?",
            "GitHub is computing".into(),
            theme::dim(),
            theme::dim(),
        ),
        (MergeState::Blocked, None) => (
            "⊘",
            "blocked by branch rules".into(),
            theme::red(),
            theme::red(),
        ),
        (_, None) => up_to_date(base.clone()),
    };
    let main = Item::new(vec![
        span(format!("{glyph} "), glyph_style),
        span(text, text_style),
    ]);
    let mut items = vec![main];
    if row.stack_ready.is_some() {
        let mut note = Item::new(vec![span(
            "GitHub marks this PR blocked on its own; rules are checked when the stack merges",
            theme::dim(),
        )]);
        note.full_width = true;
        items.push(note);
    }
    if pr.merge != MergeState::Dirty {
        items.push(Item::new(vec![
            span("✓ ", theme::green()),
            span("no conflicts", Style::new()),
        ]));
    }
    Section {
        title: "Merge",
        items,
    }
}

fn changed_section(row: &RowView) -> Option<Section> {
    (!row.change_lines.is_empty()).then(|| Section {
        title: "Changed",
        items: row
            .change_lines
            .iter()
            .map(|l| Item::new(vec![span(l.clone(), Style::new())]))
            .collect(),
    })
}

// ---------------------------------------------------------------- layout

/// `spans` cut to `width` columns (ending in `…` if cut) and padded to exactly `width`.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = s.width();
        if used + w <= width {
            used += w;
            out.push(s);
            continue;
        }
        let rest = width - used;
        out.push(Span::styled(fit(&s.content, rest), s.style));
        break;
    }
    // `fit` may stop a column short on a wide character.
    let used: usize = out.iter().map(Span::width).sum();
    out.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    out
}

fn dim(text: &str) -> Span<'static> {
    span(text, theme::dim())
}

/// One item as a single run of spans, the right part after its gap.
fn inline(item: Item) -> Vec<Span<'static>> {
    let mut spans = item.left;
    if let Some(right) = item.right {
        spans.push(Span::raw(item.gap));
        spans.push(right);
    }
    spans
}

fn side_by_side(
    sections: Vec<Section>,
    mode: Mode,
    width: usize,
    closing: bool,
) -> Vec<Line<'static>> {
    let (pre, cols): (&str, [usize; 3]) = match mode {
        Mode::Wide => ("     │  ", [32, 36, 36]),
        _ => {
            let last = width.saturating_sub(6 + 2 + 20 + 24 + 4).max(24);
            ("    │ ", [20, 24, last])
        }
    };
    let pre = if closing {
        pre.replace('│', " ")
    } else {
        pre.to_string()
    };
    let mut sections = sections;
    let mut notes = Vec::new();
    for s in &mut sections {
        let (full, rest): (Vec<Item>, Vec<Item>) = std::mem::take(&mut s.items)
            .into_iter()
            .partition(|i| i.full_width);
        s.items = rest;
        notes.extend(full);
    }
    let extra: Vec<Section> = sections.split_off(3.min(sections.len()));
    let mut cols: Vec<(usize, Section)> = cols.into_iter().zip(sections).collect();
    let rows = cols.iter().map(|(_, s)| s.items.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    let line = |lead: &str, cells: Vec<Vec<Span<'static>>>| {
        let mut spans = vec![dim(&pre), dim(lead)];
        for (i, cell) in cells.into_iter().enumerate() {
            if i > 0 {
                spans.push(dim("│ "));
            }
            spans.extend(cell);
        }
        Line::from(spans)
    };
    let heads = cols
        .iter()
        .map(|(w, s)| clip(vec![span(s.title, theme::bold())], *w))
        .collect();
    out.push(line("┌ ", heads));
    let mut bodies: Vec<std::vec::IntoIter<Item>> = cols
        .iter_mut()
        .map(|(_, s)| std::mem::take(&mut s.items).into_iter())
        .collect();
    for _ in 0..rows {
        let cells = bodies
            .iter_mut()
            .zip(&cols)
            .map(|(items, (w, _))| clip(items.next().map(inline).unwrap_or_default(), *w))
            .collect();
        out.push(line("│ ", cells));
    }
    let total = width.saturating_sub(dw(&pre) + 2);
    for note in notes {
        out.push(line("│ ", vec![clip(inline(note), total)]));
    }
    for s in extra {
        out.push(line(
            "│ ",
            vec![clip(vec![span(s.title, theme::bold())], total)],
        ));
        for item in s.items {
            out.push(line("│ ", vec![clip(inline(item), total)]));
        }
    }
    out.push(close_line(&pre, "└ "));
    out
}

fn close_line(pre: &str, corner: &str) -> Line<'static> {
    Line::from(vec![
        dim(pre),
        dim(corner),
        span("␣", theme::bold()),
        dim(" close"),
    ])
}

fn stacked(sections: Vec<Section>, width: usize, closing: bool) -> Vec<Line<'static>> {
    let rail = if closing { "    " } else { " │  " };
    let mut out = Vec::new();
    for (i, s) in sections.into_iter().enumerate() {
        let corner = if i == 0 { "┌ " } else { "├ " };
        let used = dw(rail) + dw(corner) + dw(s.title) + 1;
        out.push(Line::from(vec![
            dim(rail),
            dim(corner),
            span(s.title, theme::bold()),
            dim(&format!(" {}", "─".repeat(width.saturating_sub(used)))),
        ]));
        for item in s.items {
            let lead = format!("{rail}│ ");
            let room = width.saturating_sub(dw(&lead));
            let mut spans = item.left;
            if let Some(right) = item.right {
                // Right-aligned with one column to spare, as on board 2.
                let left_w: usize = spans.iter().map(Span::width).sum();
                let gap = room.saturating_sub(left_w + right.width() + 1).max(1);
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(right);
            }
            let mut line = vec![dim(&lead)];
            line.extend(clip(spans, room));
            out.push(Line::from(line));
        }
    }
    out.push(close_line(rail, "└ "));
    out
}

/// The drawer under `row`: CI, Reviews and Merge (and Changed when there is something).
pub fn drawer_lines(row: &RowView, width: u16, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let mode = Mode::of(width);
    let w = width as usize;
    let mut sections = vec![
        ci_section(row, mode, now),
        reviews_section(row, mode),
        merge_section(row, mode),
    ];
    sections.extend(changed_section(row));
    // The rail under the tree stops after the last layer.
    let closing = row.is_last;
    if mode.side_by_side() {
        side_by_side(sections, mode, w, closing)
    } else {
        stacked(sections, w, closing)
    }
}
