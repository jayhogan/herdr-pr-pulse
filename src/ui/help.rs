//! The `?` overlay: the legend of every glyph and the key list (board 5). Two columns from
//! 100 columns up, one below; the color word column only when a column is 60 wide.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::attention::{AgentState, Attention};
use crate::ui::cells::dw;
use crate::ui::theme;

const GLYPH_W: usize = 10;
const COLOR_W: usize = 12;

struct Entry {
    glyph: Vec<Span<'static>>,
    color: &'static str,
    meaning: &'static str,
}

struct Section {
    title: &'static str,
    entries: Vec<Entry>,
}

fn e(glyph: &'static str, style: Style, color: &'static str, meaning: &'static str) -> Entry {
    Entry {
        glyph: vec![Span::styled(glyph, style)],
        color,
        meaning,
    }
}

fn att(a: Attention, color: &'static str, meaning: &'static str) -> Entry {
    let (g, s) = theme::attention(a);
    e(g, s, color, meaning)
}

fn dot(a: AgentState, color: &'static str, meaning: &'static str) -> Entry {
    let (g, s) = theme::agent(a);
    e(g, s, color, meaning)
}

fn sections() -> (Vec<Section>, Vec<Section>, Section) {
    let plain = Style::default();
    let bold_red = theme::red_bold();
    let left = vec![
        Section {
            title: "ATTENTION  (gutter; stacks sort by worst layer)",
            entries: vec![
                att(
                    Attention::Urgent,
                    "red bg",
                    "urgent: CI failing + agent idle",
                ),
                att(
                    Attention::NeedsYou,
                    "red bold",
                    "needs you: CI ✗, changes, conflicts",
                ),
                att(
                    Attention::Nudge,
                    "yellow",
                    "nudge: behind base, threads, no reviewer",
                ),
                att(Attention::Ready, "green bold", "ready to merge now"),
                att(
                    Attention::Waiting,
                    "dim",
                    "waiting: lower layer, reviews or CI",
                ),
            ],
        },
        Section {
            title: "STATE",
            entries: vec![
                e("●", plain, "default", "ready for review"),
                e("◌", theme::dim(), "dim", "draft"),
            ],
        },
        Section {
            title: "CI",
            entries: vec![
                e(
                    "✓ pass",
                    theme::green(),
                    "green",
                    "all checks pass (n/m shown)",
                ),
                e(
                    "✗ name",
                    bold_red,
                    "red bold",
                    "failing; first failed check named",
                ),
                e(
                    "◐ n/m",
                    theme::cyan(),
                    "cyan",
                    "running; n of m checks done",
                ),
                e(
                    "– none",
                    theme::dim(),
                    "dim",
                    "no checks configured or pushed",
                ),
                e("·1 opt ✗", theme::dim(), "dim", "optional check failing"),
            ],
        },
        Section {
            title: "THREADS",
            entries: vec![
                e(
                    "◇ n",
                    theme::yellow(),
                    "yellow",
                    "n unresolved review threads",
                ),
                e("· 0", theme::dim(), "dim", "none unresolved"),
            ],
        },
        Section {
            title: "REVIEW",
            entries: vec![
                e("✓", theme::green(), "green", "approved"),
                e("✗", theme::red(), "red", "changes requested"),
                e(
                    "○ n",
                    theme::dim(),
                    "dim",
                    "pending; n reviewers yet to review",
                ),
                e(
                    "∅",
                    theme::yellow(),
                    "yellow",
                    "no reviewers assigned (dim on drafts)",
                ),
            ],
        },
    ];
    let right = vec![
        Section {
            title: "MERGE  (upper layers show waiting instead)",
            entries: vec![
                e("✓", theme::green(), "green", "clean, can merge"),
                e(
                    "⊘",
                    theme::yellow(),
                    "yellow",
                    "blocked by branch rules (e.g. review)",
                ),
                e("↓ n", theme::yellow(), "yellow", "behind base by n commits"),
                e("⚠", bold_red, "red bold", "conflicts with base"),
                e("?", theme::dim(), "dim", "unknown, GitHub still computing"),
                e(
                    "◷ #n",
                    theme::dim(),
                    "dim",
                    "waiting on #n below it in the stack",
                ),
            ],
        },
        Section {
            title: "AGENT DOT  (Workspace view, end of PR line 1)",
            entries: vec![
                dot(AgentState::Working, "green", "working"),
                dot(
                    AgentState::Blocked,
                    "yellow bold",
                    "blocked, waiting on your input",
                ),
                dot(
                    AgentState::Idle,
                    "dim",
                    "idle (red row if its CI is failing)",
                ),
                dot(
                    AgentState::Unknown,
                    "dim",
                    "agent state unknown (herdr not reachable)",
                ),
            ],
        },
        Section {
            title: "STACKS",
            entries: vec![
                e(
                    "▾ ▸",
                    theme::bold(),
                    "bold",
                    "expanded / collapsed  (h/l or ←/→)",
                ),
                e("╭─", theme::dim(), "dim", "base branch (main)"),
                e("├─ n", theme::dim(), "dim", "layer n, counted up from main"),
                Entry {
                    glyph: [
                        Attention::Ready,
                        Attention::NeedsYou,
                        Attention::Waiting,
                        Attention::Waiting,
                    ]
                    .into_iter()
                    .map(|a| {
                        let (g, s) = theme::attention(a);
                        Span::styled(g, s)
                    })
                    .collect(),
                    color: "mixed",
                    meaning: "minimap, one glyph per layer, base first",
                },
            ],
        },
        Section {
            title: "CHROME",
            entries: vec![
                e("▶", theme::bold(), "bold", "cursor row"),
                e("✦", theme::bold(), "bold", "changed since you looked"),
                e(
                    " bar ",
                    theme::bar(),
                    "reverse",
                    "header: view, scope, refresh time",
                ),
                e(
                    "⠹",
                    theme::cyan(),
                    "cyan",
                    "fetching; ✗ offline + stale age",
                ),
            ],
        },
    ];
    let k = |g: &'static str, m: &'static str| e(g, theme::bold(), "", m);
    let keys = Section {
        title: "KEYS",
        entries: vec![
            k("j/k ↑/↓", "move"),
            k("⏎", "jump to agent pane"),
            k("o", "open PR in browser"),
            k("r", "refresh"),
            k("s", "authored/assigned (Mine)"),
            k("h/l ←/→", "fold/unfold stack"),
            k("␣", "details drawer"),
            k("/", "filter"),
            k("S", "sort"),
            k("?", "help (close with ? or Esc)"),
            k("q", "quit"),
        ],
    };
    (left, right, keys)
}

/// Word-wraps `s` to `width` columns (at least one word per line).
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in s.split(' ').filter(|w| !w.is_empty()) {
        match out.last_mut() {
            Some(l) if dw(l) + 1 + dw(word) <= width => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    out
}

fn section_lines(s: &Section, colw: usize, show_color: bool) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = wrap(s.title, colw.saturating_sub(2).max(8))
        .into_iter()
        .map(|t| Line::styled(format!(" {t}"), theme::bold()))
        .collect();
    let mw = colw
        .saturating_sub(GLYPH_W + if show_color { COLOR_W } else { 0 })
        .max(8);
    for en in &s.entries {
        let mut chunks = wrap(en.meaning, mw).into_iter();
        let mut spans = vec![Span::raw(" ")];
        spans.extend(en.glyph.iter().cloned());
        let used: usize = en.glyph.iter().map(|g| g.width()).sum();
        spans.push(Span::raw(
            " ".repeat((GLYPH_W - 1).saturating_sub(used).max(1)),
        ));
        if show_color {
            let pad = COLOR_W.saturating_sub(dw(en.color));
            spans.push(Span::styled(
                format!("{}{}", en.color, " ".repeat(pad)),
                theme::dim(),
            ));
        }
        spans.push(Span::raw(chunks.next().unwrap_or_default()));
        out.push(Line::from(spans));
        let indent = " ".repeat(GLYPH_W + if show_color { COLOR_W } else { 0 });
        out.extend(chunks.map(|c| Line::raw(format!("{indent}{c}"))));
    }
    out
}

fn stack(secs: &[Section], colw: usize, show_color: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (i, s) in secs.iter().enumerate() {
        if i > 0 {
            out.push(Line::raw(""));
        }
        out.extend(section_lines(s, colw, show_color));
    }
    out
}

/// Side by side: `left` padded to `colw`, then `right`.
fn zip(left: Vec<Line<'static>>, right: Vec<Line<'static>>, colw: usize) -> Vec<Line<'static>> {
    (0..left.len().max(right.len()))
        .map(|i| {
            let mut l = left.get(i).cloned().unwrap_or_default();
            let pad = colw.saturating_sub(l.width());
            l.spans.push(Span::raw(" ".repeat(pad)));
            l.spans
                .extend(right.get(i).cloned().unwrap_or_default().spans);
            l
        })
        .collect()
}

/// The legend and key list for a terminal `width` columns wide.
pub fn help_lines(width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let (left, right, keys) = sections();
    if width >= 100 {
        let colw = width / 2;
        let show_color = colw >= 60;
        let mut out = zip(
            stack(&left, colw, show_color),
            stack(&right, colw, show_color),
            colw,
        );
        out.push(Line::styled("─".repeat(width), theme::dim()));
        let half = keys.entries.len().div_ceil(2);
        let mut entries = keys.entries;
        let second = entries.split_off(half);
        let a = Section {
            title: keys.title,
            entries,
        };
        let b = Section {
            title: "",
            entries: second,
        };
        let mut bl = section_lines(&b, colw, false);
        bl.insert(0, Line::raw(""));
        out.extend(zip(section_lines(&a, colw, false), bl, colw));
        out
    } else {
        let mut all = left;
        all.extend(right);
        all.push(keys);
        stack(&all, width, false)
    }
}
