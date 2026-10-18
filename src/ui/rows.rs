//! Turns the `Screen` view model into terminal lines (spec §10, boards 1-3).
//!
//! Wide/Medium draw one line per PR (boards 1 and 3); Narrow/Tiny draw two (board 2, with the
//! 34-column words below 40). All widths are display widths, never byte lengths.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{Body, GroupView, RowView, Screen};
use crate::attention::{Attention, Reason, reasons};
use crate::stacks::GroupKind;
use crate::ui::cells::{
    cell, ci, dw, first_class, merge, pick, plural, reason_style, reason_words, review, threads,
    tree,
};
use crate::ui::chrome::{footer, top_lines};
use crate::ui::theme;

pub use crate::ui::cells::fit;

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

/// Column widths for one-line rows: cursor, attention, layer, #, state, CI, threads,
/// review, merge. The title takes what is left.
pub(super) fn widths(d: Density) -> [usize; 9] {
    match d {
        Wide => [2, 3, 6, 7, 8, 21, 6, 15, 21],
        _ => [2, 2, 4, 6, 2, 14, 4, 6, 18],
    }
}

pub(super) fn title_width(d: Density, width: usize) -> usize {
    width.saturating_sub(widths(d).iter().sum())
}

// ---------------------------------------------------------------- group headers

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
