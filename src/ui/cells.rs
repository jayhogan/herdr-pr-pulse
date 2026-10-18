//! Text for each cell of a PR row, at each density: CI, threads, review, merge, layer tree,
//! plus the width-aware helpers every renderer shares.

use chrono::{DateTime, Utc};
use ratatui::style::Style;

use crate::ago;
use ratatui::text::Span;

use crate::app::{GroupView, RowView};
use crate::attention::{Attention, Reason};
use crate::model::{CiState, MergeState, Pr, PrState, ReviewState};
use crate::stacks::GroupKind;
use crate::ui::rows::Density::{self, *};
use crate::ui::theme;

/// One word per density: [Wide, Medium, Narrow, Tiny].
pub(super) fn pick(d: Density, words: [&str; 4]) -> String {
    let i = match d {
        Wide => 0,
        Medium => 1,
        Narrow => 2,
        Tiny => 3,
    };
    words[i].to_string()
}

pub(super) fn dw(s: &str) -> usize {
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
pub(super) fn cell(text: &str, width: usize, style: Style) -> Span<'static> {
    let t = fit(text, width.saturating_sub(1));
    let pad = width.saturating_sub(dw(&t));
    Span::styled(format!("{t}{}", " ".repeat(pad)), style)
}

pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Reasons in the same severity class as the first one (needs you / nudge / waiting).
pub(super) fn first_class(reasons: &[Reason]) -> Vec<&Reason> {
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

pub(super) fn reason_words(r: &Reason, base: &str, d: Density) -> String {
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
pub(super) fn reason_style(r: &Reason) -> Style {
    match r {
        Reason::CiFailing(_) | Reason::ChangesRequested | Reason::Conflicts => theme::red(),
        Reason::Behind(_) | Reason::Threads(_) | Reason::NoReviewers => theme::yellow(),
        _ => theme::dim(),
    }
}

pub(super) type Text = (String, Style);

pub(super) fn ci(pr: &Pr, d: Density) -> Text {
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

pub(super) fn threads(pr: &Pr, d: Density) -> Text {
    match pr.unresolved_threads {
        0 => (pick(d, ["· 0", "·", "·0", "·0"]), theme::dim()),
        n if d == Wide => (format!("◇ {n}"), theme::yellow()),
        n => (format!("◇{n}"), theme::yellow()),
    }
}

pub(super) fn review(pr: &Pr, d: Density) -> Text {
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

pub(super) fn merge(r: &RowView, d: Density, now: DateTime<Utc>) -> Text {
    let pr = &r.pr;
    match pr.state {
        PrState::Merged => {
            let t = match pr.closed_at {
                Some(at) => format!("⊙ merged {} ago", ago::short(at, now)),
                None => "⊙ merged".into(),
            };
            return (t, theme::dim());
        }
        PrState::Closed => return ("✕ closed".into(), theme::dim()),
        PrState::Open => {}
    }
    if let Some(n) = r.waiting_on {
        let t = pick(d, ["◷ waiting on #", "◷ on #", "◷ on #", "◷#"]) + &n.to_string();
        return (t, theme::dim());
    }
    if r.stack_ready.is_some() {
        let t = pick(d, ["✓ stack ok", "✓ stack ok", "✓ stack ok", "✓ stack"]);
        return (t, theme::green().patch(theme::bold()));
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

pub(super) fn tree(g: &GroupView, r: &RowView, d: Density) -> String {
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
