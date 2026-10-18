//! Loading, empty and error states (spec §11, board 4).

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ago;
use crate::app::{Banner, EmptyInfo, LoadProgress, Scope, Screen, ViewKind};
use crate::attention::AgentState;
use crate::fetch::RepoStatus;
use crate::ui::cells::{dw, plural};
use crate::ui::rows::Density::{self, *};
use crate::ui::theme;

/// Skeleton rows under the loading lines: one-line rows (Wide/Medium) or a two-line row.
const SKELETON: &str = "  ░░ ░░░░ ░░░░░░░░░░░░░░░░░░  ░░░░░░░  ░░░  ░░░░░░  ░░░░░░░░░░";
const SKELETON_NARROW: [&str; 2] = [
    " ░░ ░ ░░░░░ ░░░░░░░░░░░░░░░░░░         ░",
    "      ░░░░░░ ░░░░ ░░ ░░░░░░░",
];

/// `spans` cut to `width` columns, ending in `…` when anything was cut.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(Span::width).sum();
    if total <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = s.width();
        if used + w < width {
            used += w;
            out.push(s);
            continue;
        }
        let room = width - used;
        let mut t = String::new();
        let mut u = 0;
        for c in s.content.chars() {
            let cw = dw(c.encode_utf8(&mut [0; 4]));
            if u + cw > room.saturating_sub(1) {
                break;
            }
            t.push(c);
            u += cw;
        }
        if room > 0 {
            t.push('…');
        }
        out.push(Span::styled(t, s.style));
        break;
    }
    out
}

fn line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    Line::from(clip(spans, width))
}

/// Words of `text` in `style`; text between backticks is bold and loses the backticks.
fn words(text: &str, style: Style) -> Vec<(String, Style, bool)> {
    let mut out: Vec<(String, Style, bool)> = Vec::new();
    let mut prev_ends_ws = true;
    for (i, part) in text.split('`').enumerate() {
        let s = if i % 2 == 1 {
            style.patch(theme::bold())
        } else {
            style
        };
        // No space at a backtick boundary (e.g. "`gh auth refresh`,"): glue the words.
        let glued = !prev_ends_ws && !part.starts_with(char::is_whitespace);
        prev_ends_ws = part.is_empty() || part.ends_with(char::is_whitespace);
        for (j, w) in part.split_whitespace().enumerate() {
            // Punctuation right after a closing backtick stays on the word before it.
            let glue = j == 0 && glued && !out.is_empty();
            out.push((w.to_string(), s, glue));
        }
    }
    out
}

/// `words` laid out after `prefix`, continuation lines indented as deep. With `one_line`
/// everything goes on the first line, cut with `…`.
fn wrap(
    prefix: Vec<Span<'static>>,
    words: Vec<(String, Style, bool)>,
    width: usize,
    one_line: bool,
) -> Vec<Line<'static>> {
    let indent: usize = prefix.iter().map(Span::width).sum();
    let mut lines = vec![prefix];
    let mut used = indent;
    let mut at_start = true;
    for (text, style, glue) in words {
        let w = dw(&text);
        if !one_line && !glue && !at_start && used + 1 + w > width {
            lines.push(vec![Span::raw(" ".repeat(indent))]);
            used = indent;
            at_start = true;
        }
        let cur = lines.last_mut().unwrap();
        if !glue && !at_start {
            cur.push(Span::raw(" "));
            used += 1;
        }
        cur.push(Span::styled(text, style));
        used += w;
        at_start = false;
    }
    lines.into_iter().map(|l| line(l, width)).collect()
}

/// The red banner (board 4): what went wrong and how old the data on screen is, `r retry`
/// on the right, and the fix on the next line(s). Narrow/Tiny keep both to one line each.
pub(super) fn banner_lines(
    screen: &Screen,
    b: &Banner,
    d: Density,
    width: usize,
) -> Vec<Line<'static>> {
    let narrow = matches!(d, Narrow | Tiny);
    let has_fix = !b.fix.is_empty();
    let mut left = format!(" ✗ {}", b.message);
    if has_fix {
        left.push('.');
    }
    let right = if narrow { "r " } else { "r retry " };
    let room = width.saturating_sub(dw(right) + 1);
    if let (false, true, Some(at)) = (narrow, screen.header.stale, screen.header.fetched_at) {
        let clock = at.with_timezone(&screen.clock).format("%H:%M");
        let age = ago::short(at, screen.now);
        // The longest wording that fits, so the data's age is never cut off.
        let tails = [
            format!(" Showing data from {clock} ({age} old)."),
            format!(" Data from {clock} ({age} old)."),
            format!(" ({age} old)"),
        ];
        let tail = tails
            .iter()
            .find(|t| dw(&left) + dw(t) <= room)
            .unwrap_or(&tails[2]);
        left.push_str(tail);
    }
    let left = crate::ui::cells::fit(&left, room);
    let gap = width.saturating_sub(dw(&left) + dw(right));
    let mut out =
        vec![Line::raw(format!("{left}{}{right}", " ".repeat(gap))).style(theme::hot_row())];
    if has_fix {
        let prefix = vec![Span::raw("   Fix: ")];
        out.extend(wrap(prefix, words(&b.fix, Style::new()), width, narrow));
    }
    out
}

fn skeleton(d: Density) -> Vec<Line<'static>> {
    let rows = match d {
        Wide | Medium => [SKELETON, SKELETON],
        Narrow | Tiny => SKELETON_NARROW,
    };
    rows.iter()
        .map(|r| Line::styled(r.to_string(), theme::dim()))
        .collect()
}

/// "2 of 3 repos": the repo being read (or the last one done) out of all.
fn repo_count(p: &LoadProgress) -> String {
    let done = p
        .repos
        .iter()
        .filter(|(_, s)| matches!(s, RepoStatus::Done(_)))
        .count();
    let busy = p.repos.iter().any(|(_, s)| *s == RepoStatus::Loading);
    format!("{} of {} repos", done + usize::from(busy), p.repos.len())
}

/// The first fetch: one line per repo (✓ done, spinner while read, · queued), then skeleton
/// rows. The Workspace view says what it knows about its panes first.
pub(super) fn loading_lines(
    screen: &Screen,
    p: &LoadProgress,
    d: Density,
    width: usize,
) -> Vec<Line<'static>> {
    let spin = screen.header.spinner;
    let spinner = theme::cyan().patch(theme::bold());
    let mut out = Vec::new();
    // An error before any data: nothing is on its way until `r`.
    let failed = screen.header.stale && !screen.header.fetching && p.repos.is_empty();
    let no_data = || {
        vec![
            Span::styled("✗ ", theme::red_bold()),
            Span::styled("No PR data yet.", theme::bold()),
            Span::styled("  Press r to retry.", theme::dim()),
        ]
    };
    if screen.header.view == ViewKind::Workspace {
        let (glyph, style) = if failed {
            ("✗", theme::red_bold())
        } else {
            (spin, spinner)
        };
        out.push(line(
            vec![
                Span::styled(format!(" {glyph} "), style),
                Span::styled("Matching panes to PRs", theme::bold()),
            ],
            width,
        ));
        let panes = match screen.agent_panes {
            Some(n) => vec![
                Span::styled("   ✓ ", theme::green()),
                Span::styled(
                    format!(
                        "{} in this workspace",
                        plural(n, "agent pane", "agent panes")
                    ),
                    theme::dim(),
                ),
            ],
            None if screen.herdr_down => vec![
                Span::styled("   ? ", theme::dim()),
                Span::styled("herdr not answering", theme::dim()),
            ],
            None => vec![
                Span::styled(format!("   {spin} "), theme::cyan()),
                Span::styled("reading herdr panes", theme::dim()),
            ],
        };
        out.push(line(panes, width));
        if failed {
            let mut l = vec![Span::raw("   ")];
            l.extend(no_data());
            out.push(line(l, width));
            return out;
        }
        let mut gh = "looking up PRs on GitHub".to_string();
        if !p.repos.is_empty() {
            gh.push_str(&format!(" · {}", repo_count(p)));
        }
        out.push(line(
            vec![
                Span::styled(format!("   {spin} "), theme::cyan()),
                Span::styled(gh, theme::dim()),
            ],
            width,
        ));
        out.push(line(
            vec![
                Span::styled("   · ", theme::dim()),
                Span::styled("checks + reviews queued", theme::dim()),
            ],
            width,
        ));
        out.extend(skeleton(d));
        return out;
    }

    if failed {
        let mut l = vec![Span::raw("  ")];
        l.extend(no_data());
        out.push(line(l, width));
        return out;
    }
    let mut title = vec![
        Span::styled(format!("  {spin} "), spinner),
        Span::styled("Fetching your pull requests", theme::bold()),
    ];
    if !p.repos.is_empty() {
        title.push(Span::styled(format!("   {}", repo_count(p)), theme::dim()));
    }
    out.push(line(title, width));
    let pad = p.repos.iter().map(|(r, _)| dw(r)).max().unwrap_or(0) + 3;
    for (repo, status) in &p.repos {
        let name = format!("{repo}{}", " ".repeat(pad - dw(repo)));
        let spans = match status {
            RepoStatus::Done(n) => vec![
                Span::styled("    ✓ ", theme::green()),
                Span::raw(name),
                Span::styled(
                    format!("{} · checks loaded", plural(*n, "PR", "PRs")),
                    theme::dim(),
                ),
            ],
            RepoStatus::Loading => vec![
                Span::styled(format!("    {spin} "), theme::cyan()),
                Span::raw(name),
                Span::styled("reading checks + stack order…", theme::dim()),
            ],
            RepoStatus::Queued => vec![
                Span::styled("    · ", theme::dim()),
                Span::styled(name, theme::dim()),
                Span::styled("queued", theme::dim()),
            ],
        };
        out.push(line(spans, width));
    }
    out.extend(skeleton(d));
    out
}

/// Branches a PR does not come from, so they are not flagged `(no PR)`.
fn is_default_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master")
}

/// Panes as `● w1:p1 main`, packed several to a line when they fit.
fn pane_lines(
    panes: &[(String, String, bool, Option<AgentState>)],
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut used = 0;
    for (pane, branch, has_pr, agent) in panes {
        let (glyph, style) = theme::agent(agent.unwrap_or(AgentState::Unknown));
        let flag = if !has_pr && !is_default_branch(branch) {
            " (no PR)"
        } else {
            ""
        };
        // The branch gives way first, so the `(no PR)` flag stays readable.
        let room = width.saturating_sub(3 + dw(glyph) + dw(pane) + 2 + dw(flag));
        let branch = crate::ui::cells::fit(branch, room);
        let mut entry = vec![
            Span::styled(glyph, style),
            Span::styled(format!(" {pane} {branch}"), theme::dim()),
        ];
        if !flag.is_empty() {
            entry.push(Span::styled(flag, theme::yellow()));
        }
        let w: usize = entry.iter().map(Span::width).sum();
        match lines.last_mut() {
            Some(cur) if used + 3 + w <= width => {
                cur.push(Span::raw("   "));
                cur.extend(entry);
                used += 3 + w;
            }
            _ => {
                let mut l = vec![Span::raw("   ")];
                l.extend(entry);
                lines.push(l);
                used = 3 + w;
            }
        }
    }
    lines.into_iter().map(|l| line(l, width)).collect()
}

/// What an empty view says instead of a blank body (board 4).
pub(super) fn empty_lines(info: &EmptyInfo, herdr_down: bool, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match info {
        EmptyInfo::Mine {
            scope,
            other_scope_count,
            repos,
        } => {
            out.push(line(
                vec![
                    Span::styled("  ✓ ", theme::green().patch(theme::bold())),
                    Span::styled("Nothing is waiting on you.", theme::bold()),
                ],
                width,
            ));
            let who = match scope {
                Scope::Both => "authored by or assigned to you",
                Scope::Authored => "authored by you",
                Scope::Assigned => "assigned to you",
            };
            let text = if repos.is_empty() {
                format!("No open PRs {who}.")
            } else {
                format!("No open PRs {who} in {}.", repos.join(", "))
            };
            out.extend(wrap(
                vec![Span::raw("    ")],
                words(&text, Style::new()),
                width,
                false,
            ));
            let hint = match scope {
                Scope::Authored => Some("show PRs assigned to you"),
                Scope::Assigned => Some("show PRs you authored"),
                Scope::Both => None,
            };
            if let (Some(hint), 1..) = (hint, *other_scope_count) {
                out.push(Line::raw(""));
                out.push(line(
                    vec![
                        Span::raw("    "),
                        Span::styled("s", theme::bold()),
                        Span::raw(format!(" {hint} ")),
                        Span::styled(format!("({other_scope_count})"), theme::dim()),
                    ],
                    width,
                ));
            }
        }
        EmptyInfo::Workspace { panes } => {
            out.push(line(
                vec![
                    Span::styled(" ○ ", theme::dim()),
                    Span::styled("No PRs in this workspace", theme::bold()),
                ],
                width,
            ));
            if herdr_down {
                out.push(line(
                    vec![Span::styled(
                        "   Agent panes unknown while herdr isn't responding.",
                        theme::dim(),
                    )],
                    width,
                ));
            } else if panes.is_empty() {
                out.push(line(
                    vec![Span::styled(
                        "   No agent panes in this workspace.",
                        theme::dim(),
                    )],
                    width,
                ));
            } else {
                let none = panes.iter().all(|p| !p.2);
                let t = format!(
                    "   {}{}",
                    plural(panes.len(), "agent pane", "agent panes"),
                    if none { ", none on a PR branch:" } else { ":" }
                );
                out.push(line(vec![Span::styled(t, theme::dim())], width));
                out.extend(pane_lines(panes, width));
            }
            out.extend(wrap(
                vec![Span::raw("   ")],
                words("Open a PR from a branch to see it here.", theme::dim()),
                width,
                false,
            ));
        }
        EmptyInfo::HerdrDown => {
            out.push(line(
                vec![
                    Span::styled(" ? ", theme::dim()),
                    Span::styled("Waiting for herdr", theme::bold()),
                ],
                width,
            ));
            out.extend(wrap(
                vec![Span::raw("   ")],
                words(
                    "PR Pulse can't tell which workspace this is until herdr answers.",
                    theme::dim(),
                ),
                width,
                false,
            ));
        }
        EmptyInfo::NoCheckout => {
            out.push(line(
                vec![
                    Span::styled(" ○ ", theme::dim()),
                    Span::styled("PR Pulse isn't in a git checkout", theme::bold()),
                ],
                width,
            ));
            out.extend(wrap(
                vec![Span::raw("   ")],
                words(
                    "Open it inside a workspace's worktree to see that workspace's PRs.",
                    theme::dim(),
                ),
                width,
                false,
            ));
        }
    }
    out
}

/// Under the rows while herdr is unreachable.
pub(super) fn herdr_note(width: usize) -> Line<'static> {
    line(
        vec![Span::styled(
            "   ? = agent state unknown; ⏎ disabled",
            theme::dim(),
        )],
        width,
    )
}
