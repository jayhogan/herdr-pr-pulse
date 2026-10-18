mod cells;
mod chrome;
mod drawer;
pub mod help;
pub mod rows;
mod states;
pub mod theme;

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};
    use ratatui::text::Span;

    use super::rows;
    use crate::app::{App, ViewKind};
    use crate::model;
    use crate::store::StateDir;

    fn now() -> DateTime<Utc> {
        "2026-10-17T14:32:26Z".parse().unwrap()
    }

    fn app_with(edit: impl FnOnce(&mut Vec<model::Pr>)) -> App {
        app_with_at(edit, "2026-10-17T14:32:08Z")
    }

    /// `app_with`, its data fetched at `fetched_at`.
    fn app_with_at(edit: impl FnOnce(&mut Vec<model::Pr>), fetched_at: &str) -> App {
        let mut prs =
            model::parse_detail(include_str!("../../tests/fixtures/detail.json")).unwrap();
        edit(&mut prs);
        let s = model::parse_search(include_str!("../../tests/fixtures/search.json")).unwrap();
        let ids = |h: &[model::SearchHit]| h.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.set_prs(
            prs,
            &ids(&s.authored),
            &ids(&s.assigned),
            &s.viewer,
            fetched_at.parse().unwrap(),
        );
        // Agents come from the fixture snapshot, seen from the Mine copy's pane (w4:p1).
        let tmp = tempfile::tempdir().unwrap();
        a.set_own_pane("w4:p1".into());
        a.set_path_exists(|_| true);
        let snap = crate::herdr::parse_snapshot(include_str!("../../tests/fixtures/snapshot.json"));
        a.apply_snapshot(
            snap,
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        a
    }

    /// The Workspace copy in the audit workspace (own pane w1:p2), memory seeded from the
    /// fixture, cursor on #1287 as on board 2.
    fn ws_app() -> App {
        ws_app_in(tempfile::tempdir().unwrap().path())
    }

    /// `ws_app` with its state (memory) in `dir`.
    fn ws_app_in(dir: &std::path::Path) -> App {
        let prs = model::parse_detail(include_str!("../../tests/fixtures/detail.json")).unwrap();
        let s = model::parse_search(include_str!("../../tests/fixtures/search.json")).unwrap();
        let ids = |h: &[model::SearchHit]| h.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_prs(
            prs,
            &ids(&s.authored),
            &ids(&s.assigned),
            &s.viewer,
            "2026-10-17T14:32:08Z".parse().unwrap(),
        );
        std::fs::write(
            dir.join(crate::workspaces::FILE),
            include_str!("../../tests/fixtures/workspaces.json"),
        )
        .unwrap();
        a.set_own_pane("w1:p2".into());
        a.set_path_exists(|_| true);
        let snap = crate::herdr::parse_snapshot(include_str!("../../tests/fixtures/snapshot.json"));
        a.apply_snapshot(
            snap,
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(dir.to_path_buf()),
            now(),
        );
        a.move_cursor(2);
        a
    }

    /// The fixture app with the cursor on #1287, as on boards 1 and 3.
    fn app() -> App {
        let mut a = app_with(|_| {});
        a.move_cursor(2);
        a
    }

    /// After `s` the header says which half of Mine is listed, at every density.
    #[test]
    fn header_names_a_narrowed_scope() {
        let mut a = app();
        // Both halves: only Wide has room to say so.
        for width in [80, 44, 34] {
            let header = text_lines(&draw(&a, width, 20))[0].clone();
            assert!(
                !header.contains("auth") && !header.contains("as"),
                "{width}: {header}"
            );
        }
        for (want, tiny) in [("authored", "auth"), ("assigned", "asgn")] {
            a.cycle_scope();
            for width in [120, 80, 44] {
                let header = text_lines(&draw(&a, width, 20))[0].clone();
                assert!(header.contains(want), "{width}: {header}");
            }
            let header = text_lines(&draw(&a, 34, 20))[0].clone();
            assert!(header.contains(tiny), "34: {header}");
        }
    }

    fn draw(a: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let screen = a.screen(now());
        let mut scroll = 0;
        terminal
            .draw(|f| {
                rows::render(f, &screen, &mut scroll);
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

    /// Row index of the first line containing `needle`.
    fn line_of(buf: &Buffer, needle: &str) -> usize {
        text_lines(buf)
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not on screen:\n{}", text(buf)))
    }

    #[test]
    fn stale_header_and_banner_are_visible() {
        use crate::fetch::FetchEvent;
        use crate::store::Cache;
        let mut a = app();
        let cache = Cache {
            error: Some(crate::github::GhError::LoginExpired),
            ..Default::default()
        };
        a.apply_fetch(FetchEvent::Done(cache), now());
        for width in [120, 80, 44, 34] {
            let buf = draw(&a, width, 30);
            let lines = text_lines(&buf);
            assert!(
                lines[0].contains("✗ offline · stale 18s"),
                "{width}: {}",
                lines[0]
            );
            assert!(text(&buf).contains("✗ GitHub login expired"), "{width}");
            assert!(text(&buf).contains("#1287"), "{width}: rows kept");
        }
    }

    #[test]
    fn loading_lists_repos() {
        use crate::fetch::{FetchEvent, RepoStatus};
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        spin(&mut a, 2);
        assert!(
            !text(&draw(&a, 80, 10)).contains("loading…"),
            "not fetching yet"
        );
        a.apply_fetch(FetchEvent::Started, now());
        assert!(text(&draw(&a, 80, 10)).contains("⠹ loading…"));
        assert!(text(&draw(&a, 80, 10)).contains("⠹ Fetching your pull requests"));
        a.apply_fetch(
            FetchEvent::Progress(vec![
                ("heard/docket".into(), RepoStatus::Done(2)),
                ("heard/heard-ts".into(), RepoStatus::Loading),
                ("heard/web".into(), RepoStatus::Queued),
            ]),
            now(),
        );
        let t = text(&draw(&a, 80, 10));
        assert!(t.contains("✓ heard/docket"), "{t}");
        assert!(t.contains("⠹ heard/heard-ts"), "{t}");
        assert!(t.contains("· heard/web"), "{t}");
    }

    /// Board 3: the thread-model stack folded, cursor on its header.
    #[test]
    fn mine_80_folded() {
        let mut a = app();
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        while a.selected_group().is_none_or(|g| g.name != "thread-model") {
            a.handle_key(key('j'));
        }
        a.handle_key(key('h'));
        let buf = draw(&a, 80, 30);
        assert!(text(&buf).contains("(2 hidden)"));
        insta::assert_snapshot!(text(&buf));
    }

    #[test]
    fn mine_44_filter_editing() {
        let mut a = app();
        a.handle_key(key('/'));
        for c in "thread".chars() {
            a.handle_key(key(c));
        }
        let buf = draw(&a, 44, 30);
        assert!(text_lines(&buf).iter().any(|l| l == "/ thread▏"));
        insta::assert_snapshot!(text(&buf));
    }

    #[test]
    fn mine_filter_applied_is_dim_line() {
        let mut a = app();
        a.handle_key(key('/'));
        a.handle_key(key('4'));
        a.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(text_lines(&draw(&a, 80, 30)).iter().any(|l| l == "/ 4"));
    }

    #[test]
    fn help_120() {
        let mut a = app();
        a.handle_key(key('?'));
        let buf = draw(&a, 120, 50);
        let t = text(&buf);
        assert!(t.contains("details drawer"), "keys not clipped:\n{t}");
        assert!(
            t.contains("ATTENTION") && t.contains("MERGE"),
            "two columns:\n{t}"
        );
        assert!(!t.contains("Tab"));
        insta::assert_snapshot!(t);
    }

    #[test]
    fn help_44() {
        let mut a = app();
        a.handle_key(key('?'));
        insta::assert_snapshot!(text(&draw(&a, 44, 90)));
    }

    #[test]
    fn help_scroll_stops_at_the_real_end() {
        let mut a = app();
        a.handle_key(key('?'));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut frame = |a: &mut App| {
            let screen = a.screen(now());
            let mut s = 0;
            let mut max = 0;
            terminal
                .draw(|f| max = rows::render(f, &screen, &mut s))
                .unwrap();
            a.set_help_max(max);
            text_lines(terminal.backend().buffer())
        };
        frame(&mut a);
        for _ in 0..200 {
            a.handle_key(key('j'));
            frame(&mut a);
        }
        let end = frame(&mut a);
        a.handle_key(key('k'));
        assert_ne!(frame(&mut a)[3], end[3], "first k moves immediately");
    }

    #[test]
    fn help_hides_column_titles_and_shows_help_footer() {
        let mut a = app();
        a.handle_key(key('?'));
        let t = text(&draw(&a, 120, 50));
        assert!(!t.contains("LAYER #"));
        assert!(t.contains("j/k scroll  ? or Esc close  q quit"), "{t}");
    }

    #[test]
    fn help_scrolls_and_does_not_panic_tiny() {
        let mut a = app();
        a.handle_key(key('?'));
        for _ in 0..3 {
            a.handle_key(key('j'));
        }
        for (w, h) in [(10, 5), (1, 1), (44, 12), (100, 6)] {
            draw(&a, w, h);
        }
        let first = text_lines(&draw(&a, 44, 12))[3].clone();
        a.handle_key(key('k'));
        assert_ne!(first, text_lines(&draw(&a, 44, 12))[3]);
    }

    #[test]
    fn mine_120() {
        insta::assert_snapshot!(text(&draw(&app(), 120, 30)));
    }

    #[test]
    fn mine_100() {
        insta::assert_snapshot!(text(&draw(&app(), 100, 30)));
    }

    #[test]
    fn mine_80() {
        insta::assert_snapshot!(text(&draw(&app(), 80, 30)));
    }

    #[test]
    fn mine_44() {
        insta::assert_snapshot!(text(&draw(&app(), 44, 30)));
    }

    #[test]
    fn mine_34() {
        insta::assert_snapshot!(text(&draw(&app(), 34, 30)));
    }

    #[test]
    fn workspace_120() {
        insta::assert_snapshot!(text(&draw(&ws_app(), 120, 30)));
    }

    #[test]
    fn workspace_80() {
        insta::assert_snapshot!(text(&draw(&ws_app(), 80, 30)));
    }

    #[test]
    fn workspace_44() {
        insta::assert_snapshot!(text(&draw(&ws_app(), 44, 30)));
    }

    #[test]
    fn workspace_34() {
        insta::assert_snapshot!(text(&draw(&ws_app(), 34, 30)));
    }

    #[test]
    fn workspace_marks_checked_out_row_only() {
        for width in [120, 80, 44, 34] {
            let buf = draw(&ws_app(), width, 30);
            let lines = text_lines(&buf);
            let marked: Vec<&String> = lines.iter().filter(|l| l.contains('⎇')).collect();
            // The summary (narrow) or header bar (wide) shows the branch; one row has the mark.
            assert_eq!(marked.len(), 2, "{width}:\n{}", text(&buf));
            assert!(lines[line_of(&buf, "#1287")].contains("⎇"), "{width}");
            assert!(lines[line_of(&buf, "#1287")].contains("○"), "{width}");
            let merged = &lines[line_of(&buf, "#1270")..];
            assert!(
                merged.iter().take(2).any(|l| l.contains("⊙ merged 2d ago")),
                "{width}:\n{}",
                text(&buf)
            );
        }
    }

    #[test]
    fn merged_row_is_dim() {
        for width in [120, 44] {
            let buf = draw(&ws_app(), width, 30);
            let y = line_of(&buf, "#1270") as u16;
            for x in 0..width {
                let c = &buf[(x, y)];
                if c.symbol().trim().is_empty() {
                    continue;
                }
                assert_eq!(c.fg, Color::DarkGray, "{width}: cell {x} '{}'", c.symbol());
            }
        }
    }

    #[test]
    fn mine_dot_in_header_not_row() {
        let buf = draw(&app(), 120, 30);
        let lines = text_lines(&buf);
        assert!(lines[line_of(&buf, "· audit")].contains("○"));
        assert!(!lines[line_of(&buf, "#1287")].contains("○"));
        assert!(!text(&buf).contains('⎇'));
    }

    #[test]
    fn herdr_unreachable_shows_question_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app_in(tmp.path());
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        for width in [120, 44] {
            let buf = draw(&a, width, 30);
            let lines = text_lines(&buf);
            let header = &lines[line_of(&buf, "audit")];
            assert!(header.contains('?'), "{width}: {header}");
        }
    }

    #[test]
    fn status_line_replaces_hints() {
        let mut a = app();
        a.set_status("no agent pane for this stack".into());
        for width in [120, 44] {
            let buf = draw(&a, width, 30);
            let last = text_lines(&buf).pop().unwrap();
            assert!(
                last.contains("no agent pane for this stack"),
                "{width}: {last}"
            );
        }
    }

    #[test]
    fn density_breakpoints() {
        use rows::Density::*;
        let d: Vec<_> = [200, 120, 119, 80, 79, 40, 39, 1]
            .into_iter()
            .map(rows::density)
            .collect();
        assert_eq!(d, [Wide, Wide, Medium, Medium, Narrow, Narrow, Tiny, Tiny]);
    }

    #[test]
    fn fit_uses_display_width() {
        assert_eq!(rows::fit("abcdef", 4), "abc…");
        assert_eq!(rows::fit("abc", 3), "abc");
        assert_eq!(rows::fit("日本語", 4), "日…");
        assert_eq!(rows::fit("abc", 0), "");
    }

    #[test]
    fn urgent_row_is_red_background() {
        let buf = draw(&app(), 120, 30);
        let y = line_of(&buf, "#1287") as u16;
        for x in 0..120 {
            assert_eq!(buf[(x, y)].bg, Color::Red, "cell {x} of the #1287 line");
        }
    }

    #[test]
    fn no_forbidden_colors() {
        let allowed = [
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Cyan,
            Color::DarkGray,
            Color::White,
            Color::Reset,
        ];
        for width in [120, 80, 44, 34] {
            let buf = draw(&app(), width, 30);
            for c in &buf.content {
                assert!(allowed.contains(&c.fg), "{width}: fg {:?}", c.fg);
                assert!(allowed.contains(&c.bg), "{width}: bg {:?}", c.bg);
            }
        }
    }

    #[test]
    fn wide_chars_keep_columns_aligned() {
        let title: String = "日本語のタイトル🙂…".chars().cycle().take(40).collect();
        let a = app_with(|prs| {
            prs.iter_mut().find(|p| p.number == 1284).unwrap().title = title;
        });
        let buf = draw(&a, 120, 30);
        let ci_x = |needle: &str| {
            let y = line_of(&buf, needle) as u16;
            (0..120)
                .find(|&x| ["✓", "✗", "◐", "–"].contains(&buf[(x, y)].symbol()))
                .unwrap()
        };
        let expected = ci_x("#1291");
        for n in ["#1284", "#1287", "#1295", "#412"] {
            assert_eq!(ci_x(n), expected, "CI column of {n}");
        }
    }

    #[test]
    fn tiny_workspace_on_merged_row_does_not_panic() {
        let mut a = ws_app();
        a.move_cursor(100); // the merged #1270 row is last
        let buf = draw(&a, 44, 30);
        assert!(text_lines(&buf)[line_of(&buf, "#1270")].contains('▶'));
        for width in 1..=4 {
            for height in 1..=3 {
                draw(&a, width, height);
            }
        }
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        draw(&app(), 10, 3);
        draw(&app(), 1, 1);
    }

    #[test]
    fn cursor_scrolls_into_view() {
        let mut a = app();
        a.move_cursor(100);
        let buf = draw(&a, 120, 8);
        assert!(text(&buf).contains('▶'), "{}", text(&buf));
    }

    #[test]
    fn titles_stay_wide_at_band_edges() {
        let a = app_with(|prs| {
            prs.iter_mut().find(|p| p.number == 1284).unwrap().title = "x".repeat(200);
        });
        for width in [79, 80, 119, 120] {
            let buf = draw(&a, width, 30);
            let line = &text_lines(&buf)[line_of(&buf, "#1284")];
            // The cell is the fitted text (x… ) plus one column of gap.
            let cell = line.matches('x').count() + 2;
            assert!(cell >= 22, "{width} cols: title cell {cell}\n{line}");
        }
    }

    #[test]
    fn two_line_cursor_row_at_bottom_shows_both_lines() {
        let mut a = app();
        a.move_cursor(100);
        let buf = draw(&a, 44, 6);
        let lines = text_lines(&buf);
        let y = line_of(&buf, "#1302");
        assert!(lines[y].contains('▶'), "{}", text(&buf));
        assert!(lines[y + 1].contains("? computing"), "{}", text(&buf));
    }

    fn key(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    fn drawer_app() -> App {
        let mut a = app();
        a.handle_key(key(' '));
        a
    }

    #[test]
    fn drawer_120_on_1287() {
        insta::assert_snapshot!(text(&draw(&drawer_app(), 120, 30)));
    }

    #[test]
    fn drawer_80_on_1287() {
        insta::assert_snapshot!(text(&draw(&drawer_app(), 80, 30)));
    }

    #[test]
    fn drawer_44_on_1287() {
        insta::assert_snapshot!(text(&draw(&drawer_app(), 44, 34)));
    }

    /// The audit stack's bottom #1284 as GitHub reports a ready stack's bottom: BLOCKED.
    fn stack_blocked_app() -> App {
        app_with(|prs| {
            for p in prs.iter_mut() {
                if p.number == 1284 || p.number == 1279 {
                    p.merge = model::MergeState::Blocked;
                }
            }
        })
    }

    #[test]
    fn native_stack_blocked_bottom_reads_as_stack_ok() {
        for width in [120, 44] {
            let t = text(&draw(&stack_blocked_app(), width, 30));
            let lines: Vec<&str> = t.lines().collect();
            let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
            // Only layer 1 of 4 is ready, so the header must not read as a whole-stack merge.
            let header = if width >= 80 {
                "mergeable through layer 1 of 4"
            } else {
                "through L1 of 4"
            };
            assert!(t.contains(header), "{width}:\n{t}");
            assert!(
                !t.contains("merge stack") && !t.contains("◆ stack"),
                "{width}:\n{t}"
            );
            // Two-line rows at 44 keep the merge cell on the line after #1284.
            let row = lines[at("#1284")..at("#1287")].join("\n");
            assert!(row.contains("✓ stack ok"), "{width}: {row}");
            assert!(!row.contains("blocked"), "{width}: {row}");
            let row = lines[at("#1279")..].join("\n");
            assert!(row.contains("⊘ blocked"), "{width}: {row}");
        }
    }

    #[test]
    fn drawer_explains_stack_ready() {
        let mut a = stack_blocked_app();
        a.move_cursor(1);
        a.handle_key(key(' '));
        let t = text(&draw(&a, 120, 40));
        assert!(t.contains("✓ ready to merge as a stack (1 of 4)"), "{t}");
        assert!(
            t.contains(
                "GitHub marks this PR blocked on its own; rules are checked when the stack merges"
            ),
            "{t}"
        );
        assert!(!t.contains("blocked by branch rules"), "{t}");
    }

    #[test]
    fn drawer_wraps_the_stack_note_instead_of_clipping() {
        for width in [120, 80, 44] {
            let mut a = stack_blocked_app();
            a.move_cursor(1);
            a.handle_key(key(' '));
            let t = text(&draw(&a, width, 50));
            let flat = |s: &str| {
                s.lines()
                    .flat_map(|l| l.split(['│', '┌', '└']).next_back().map(str::to_string))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let all = flat(&t);
            for want in [
                "ready to merge as a stack (1 of 4)",
                "GitHub marks this PR blocked on its own; rules are checked when the stack merges",
            ] {
                assert!(all.contains(want), "{width}: {want}\n{t}");
            }
            if width == 44 {
                let buf = draw(&a, width, 50);
                let lines = text_lines(&buf);
                let region = lines[line_of(&buf, "#1284")..line_of(&buf, "#1287")].join("\n");
                insta::assert_snapshot!("drawer_44_stack_note", region);
            }
        }
    }

    /// Every audit layer ready in order; GitHub still reports the bottom BLOCKED.
    fn whole_stack_ready_app() -> App {
        app_with(|prs| {
            for p in prs.iter_mut() {
                match p.number {
                    1284 => p.merge = model::MergeState::Blocked,
                    1287 | 1291 | 1295 => {
                        p.ci = model::Ci::from_checks(Vec::new());
                        p.approved = Some(true);
                        p.changes_requested = false;
                        p.is_draft = false;
                        p.merge = model::MergeState::Clean;
                        p.unresolved_threads = 0;
                    }
                    _ => {}
                }
            }
        })
    }

    #[test]
    fn whole_ready_stack_reads_merge_stack() {
        for (width, header) in [(120, "◆ merge stack · 4 of 4"), (44, "◆ stack 4/4")] {
            let t = text(&draw(&whole_stack_ready_app(), width, 30));
            assert!(t.contains(header), "{width}:\n{t}");
        }
    }

    #[test]
    fn unknown_bottom_keeps_the_plain_header() {
        let a = app_with(|prs| {
            for p in prs.iter_mut().filter(|p| p.number == 1284) {
                p.merge = model::MergeState::Unknown;
            }
        });
        let t = text(&draw(&a, 120, 30));
        assert!(!t.contains("◆ merge stack"), "{t}");
        assert!(t.contains("mergeable through layer"), "{t}");
    }

    #[test]
    fn drawer_splits_people_and_bots() {
        for width in [120, 80, 44] {
            let t = text(&draw(&drawer_app(), width, 40));
            let lines: Vec<&str> = t.lines().collect();
            let pos = |needle: &str| lines.iter().position(|l| l.contains(needle));
            if width < 80 {
                let (people, bots) = (pos("People").unwrap(), pos("Bots").unwrap());
                let (art, claude) = (pos("@art").unwrap(), pos("@claude[bot]").unwrap());
                assert!(people < art && art < bots && bots < claude, "{width}:\n{t}");
            } else {
                assert!(t.contains("People") && t.contains("Bots"), "{width}:\n{t}");
                assert!(t.contains("@claud"), "{width}:\n{t}");
            }
        }
    }

    #[test]
    fn drawer_toggles_and_ignores_group_headers() {
        let mut a = app();
        a.move_cursor(-2); // group header
        a.handle_key(key(' '));
        assert!(!a.screen(now()).drawer_pr.is_some());
        a.move_cursor(2);
        a.handle_key(key(' '));
        assert!(a.screen(now()).drawer_pr.is_some());
        a.handle_key(key(' '));
        assert!(!a.screen(now()).drawer_pr.is_some());
    }

    #[test]
    fn drawer_stays_on_its_pr_and_moves_with_space() {
        let mut a = drawer_app();
        a.move_cursor(1);
        let buf = draw(&a, 120, 40);
        // Still under #1287 although the cursor is on #1291.
        assert!(line_of(&buf, "#1287") < line_of(&buf, "6m ago"));
        assert!(line_of(&buf, "6m ago") < line_of(&buf, "#1291"));
        a.handle_key(key(' '));
        let buf = draw(&a, 120, 40);
        assert!(line_of(&buf, "#1291") < line_of(&buf, "@kayla"));
        assert!(!text(&buf).contains("6m ago"), "{}", text(&buf));
    }

    #[test]
    fn drawer_scrolls_into_view_with_its_row() {
        let a = drawer_app();
        let buf = draw(&a, 120, 20);
        assert!(text(&buf).contains("close"), "{}", text(&buf));
    }

    #[test]
    fn drawer_draws_on_tiny_terminals() {
        for width in 1..=40 {
            draw(&drawer_app(), width, 20);
        }
    }

    #[test]
    fn drawer_without_drawer_leaves_rows_alone() {
        assert!(!text(&draw(&app(), 120, 30)).contains("close"));
    }

    #[test]
    fn drawer_shows_conflicts_for_a_dirty_pr() {
        for width in [120, 80, 44] {
            let mut a = app();
            a.move_cursor(4); // #412, conflicts with main
            a.handle_key(key(' '));
            let t = text(&draw(&a, width, 40));
            assert!(t.contains("⚠ conflicts with main"), "{width}:\n{t}");
            assert!(!t.contains("no conflicts"), "{width}:\n{t}");
        }
    }

    // ------------------------------------------------------------ board 4: states

    fn spin(a: &mut App, frames: usize) {
        for _ in 0..frames {
            a.advance_spinner();
        }
    }

    fn fetch_error(a: &mut App, e: crate::github::GhError) {
        let cache = crate::store::Cache {
            error: Some(e),
            ..Default::default()
        };
        a.apply_fetch(crate::fetch::FetchEvent::Done(cache), now());
    }

    #[test]
    fn loading_mine_80() {
        use crate::fetch::{FetchEvent, RepoStatus};
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.apply_fetch(FetchEvent::Started, now());
        a.apply_fetch(
            FetchEvent::Progress(vec![
                ("heard/heard-ts".into(), RepoStatus::Done(5)),
                ("heard/docket".into(), RepoStatus::Loading),
                ("heard/sage".into(), RepoStatus::Queued),
            ]),
            now(),
        );
        spin(&mut a, 2);
        let buf = draw(&a, 80, 14);
        let lines = text_lines(&buf);
        assert!(lines[0].ends_with("⠹ loading…"), "{}", lines[0]);
        assert!(
            lines
                .iter()
                .any(|l| l == "  ⠹ Fetching your pull requests   2 of 3 repos"),
            "{}",
            text(&buf)
        );
        assert!(lines.iter().any(|l| l.starts_with("  ░░ ░░░░")));
        insta::assert_snapshot!(text(&buf));
    }

    #[test]
    fn empty_mine_80() {
        let a = empty_mine_app();
        let buf = draw(&a, 80, 14);
        let lines = text_lines(&buf);
        assert!(lines.iter().any(|l| l == "  ✓ Nothing is waiting on you."));
        assert!(
            lines
                .iter()
                .any(|l| l == "    s show PRs assigned to you (2)"),
            "{}",
            text(&buf)
        );
        insta::assert_snapshot!(text(&buf));
    }

    /// Mine in the authored scope with nothing authored; two PRs are assigned.
    fn empty_mine_app() -> App {
        let prs = model::parse_detail(include_str!("../../tests/fixtures/detail.json")).unwrap();
        let s = model::parse_search(include_str!("../../tests/fixtures/search.json")).unwrap();
        let assigned: Vec<String> = s.assigned.iter().map(|h| h.id.clone()).collect();
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        a.set_prs(
            prs,
            &[],
            &assigned,
            &s.viewer,
            "2026-10-17T14:32:22Z".parse().unwrap(),
        );
        a.cycle_scope(); // authored: nothing open
        a
    }

    #[test]
    fn empty_mine_34() {
        insta::assert_snapshot!(text(&draw(&empty_mine_app(), 34, 14)));
    }

    #[test]
    fn error_auth_mine_80() {
        let a = error_auth_app();
        let buf = draw(&a, 80, 30);
        let lines = text_lines(&buf);
        assert!(lines[0].ends_with("✗ offline · stale 14m"), "{}", lines[0]);
        assert!(
            lines[1].starts_with(" ✗ GitHub login expired. Showing data from 14:18 (14m old)."),
            "{}",
            lines[1]
        );
        assert!(lines[1].ends_with("r retry"), "{}", lines[1]);
        assert_eq!(
            lines[2],
            "   Fix: Run gh auth login in any pane, then press r."
        );
        // The banner line is red all the way across.
        for x in 0..80 {
            assert_eq!(buf[(x, 1)].bg, Color::Red, "banner cell {x}");
        }
        insta::assert_snapshot!(text(&buf));
    }

    /// Board 4's error state: login expired, rows from a fetch 14 minutes earlier.
    fn error_auth_app() -> App {
        let mut a = app_with_at(|_| {}, "2026-10-17T14:18:26Z");
        a.move_cursor(2);
        fetch_error(&mut a, crate::github::GhError::LoginExpired);
        a
    }

    #[test]
    fn error_auth_mine_34() {
        insta::assert_snapshot!(text(&draw(&error_auth_app(), 34, 30)));
    }

    #[test]
    fn saml_banner_80_keeps_the_age() {
        let mut a = app_with_at(|_| {}, "2026-10-17T14:18:26Z");
        fetch_error(&mut a, crate::github::GhError::SamlSso);
        let buf = draw(&a, 80, 12);
        let lines = text_lines(&buf);
        assert!(lines[1].contains("(14m old)"), "{}", lines[1]);
        assert!(!lines[1].contains('…'), "{}", lines[1]);
        assert!(lines[1].ends_with("r retry"), "{}", lines[1]);
    }

    #[test]
    fn saml_banner_text() {
        let mut a = app_with_at(|_| {}, "2026-10-17T14:18:26Z");
        fetch_error(&mut a, crate::github::GhError::SamlSso);
        let buf = draw(&a, 120, 12);
        let lines = text_lines(&buf);
        assert!(
            lines[1].starts_with(
                " ✗ Token not authorized for SAML SSO. Showing data from 14:18 (14m old)."
            ),
            "{}",
            lines[1]
        );
        assert_eq!(
            lines[2],
            "   Fix: Run gh auth refresh, then authorize the token for your org at \
             github.com/settings/tokens, then press r."
        );
        insta::assert_snapshot!(text(&buf));
    }

    #[test]
    fn stale_rows_are_dim_with_glyphs_kept() {
        let mut a = app();
        fetch_error(&mut a, crate::github::GhError::LoginExpired);
        for width in [120, 80, 44] {
            let buf = draw(&a, width, 40);
            let y = line_of(&buf, "#1287") as u16;
            assert!(text_lines(&buf)[y as usize].contains("▲▲"), "{width}");
            for x in 0..width {
                let c = &buf[(x, y)];
                assert_ne!(c.bg, Color::Red, "{width}: cell {x} keeps the red row");
                if !c.symbol().trim().is_empty() {
                    assert_eq!(c.fg, Color::DarkGray, "{width}: cell {x} '{}'", c.symbol());
                }
            }
        }
    }

    #[test]
    fn header_shows_spinner_and_stale_together() {
        let mut a = app();
        fetch_error(&mut a, crate::github::GhError::LoginExpired);
        a.apply_fetch(crate::fetch::FetchEvent::Started, now());
        spin(&mut a, 2);
        for width in [120, 80, 44] {
            let l = text_lines(&draw(&a, width, 30))[0].clone();
            assert!(l.contains("⠹ ✗ offline · stale 18s"), "{width}: {l}");
        }
        // The spinner turns once per frame.
        spin(&mut a, 1);
        assert!(text_lines(&draw(&a, 80, 30))[0].contains("⠸ ✗ offline"));
    }

    /// A Workspace copy in w1 whose panes sit on branches without PRs (heard/web).
    fn no_pr_workspace(dir: &std::path::Path) -> App {
        let snap = r#"{"result":{"snapshot":{
            "workspaces":[{"workspace_id":"w1","worktree":{"checkout_path":"/work/web"}}],
            "panes":[
              {"pane_id":"w1:p1","workspace_id":"w1","cwd":"/work/web"},
              {"pane_id":"w1:p2","workspace_id":"w1","cwd":"/work/web-2"},
              {"pane_id":"w1:p3","workspace_id":"w1","cwd":"/work/web-spike"},
              {"pane_id":"w1:p4","workspace_id":"w1","cwd":"/work/web","focused":true}],
            "agents":[
              {"pane_id":"w1:p1","workspace_id":"w1","agent_status":"working"},
              {"pane_id":"w1:p2","workspace_id":"w1","agent_status":"idle"},
              {"pane_id":"w1:p3","workspace_id":"w1","agent_status":"idle"}]}}}"#;
        let branches = r#"{
            "/work/web": {"repo":"heard/web","branch":"main","toplevel":"/work/web"},
            "/work/web-2": {"repo":"heard/web","branch":"main","toplevel":"/work/web-2"},
            "/work/web-spike": {"repo":"heard/web","branch":"spike/kafka-retry","toplevel":"/work/web-spike"}}"#;
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w1:p4".into());
        a.set_path_exists(|_| true);
        a.apply_snapshot(
            crate::herdr::parse_snapshot(snap),
            &mut crate::git::FixtureBranches::from_json(branches).unwrap(),
            &StateDir::new(dir.to_path_buf()),
            now(),
        );
        a
    }

    #[test]
    fn loading_ws_44() {
        use crate::fetch::{FetchEvent, RepoStatus};
        let tmp = tempfile::tempdir().unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w1:p2".into());
        a.set_path_exists(|_| true);
        a.apply_snapshot(
            crate::herdr::parse_snapshot(include_str!("../../tests/fixtures/snapshot.json")),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        a.apply_fetch(FetchEvent::Started, now());
        a.apply_fetch(
            FetchEvent::Progress(vec![
                ("heard/docket".into(), RepoStatus::Done(2)),
                ("heard/heard-ts".into(), RepoStatus::Loading),
            ]),
            now(),
        );
        spin(&mut a, 2);
        let buf = draw(&a, 44, 14);
        let lines = text_lines(&buf);
        assert!(lines[0].ends_with("⠹ loading"), "{}", lines[0]);
        assert!(
            lines.iter().any(|l| l == " ⠹ Matching panes to PRs"),
            "{}",
            text(&buf)
        );
        insta::assert_snapshot!(text(&buf));
    }

    #[test]
    fn empty_ws_44() {
        let tmp = tempfile::tempdir().unwrap();
        let a = empty_ws_app(tmp.path());
        let buf = draw(&a, 44, 14);
        let t = text(&buf);
        assert!(t.contains(" ○ No PRs in this workspace"), "{t}");
        assert!(t.contains("w1:p3 spike/kafka-retry (no PR)"), "{t}");
        assert!(!t.contains("main (no PR)"), "{t}");
        insta::assert_snapshot!(t);
    }

    /// `no_pr_workspace` with the fixture PRs fetched: none is on its panes' branches.
    fn empty_ws_app(dir: &std::path::Path) -> App {
        let mut a = no_pr_workspace(dir);
        let prs = model::parse_detail(include_str!("../../tests/fixtures/detail.json")).unwrap();
        a.set_prs(
            prs,
            &[],
            &[],
            "jay",
            "2026-10-17T14:32:22Z".parse().unwrap(),
        );
        a
    }

    #[test]
    fn empty_ws_34() {
        let tmp = tempfile::tempdir().unwrap();
        insta::assert_snapshot!(text(&draw(&empty_ws_app(tmp.path()), 34, 14)));
    }

    fn herdr_down(a: &mut App, dir: &std::path::Path) {
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(dir.to_path_buf()),
            now(),
        );
    }

    #[test]
    fn herdr_down_ws_34() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app_in(tmp.path());
        herdr_down(&mut a, tmp.path());
        insta::assert_snapshot!(text(&draw(&a, 34, 30)));
    }

    #[test]
    fn empty_ws_with_herdr_down_says_panes_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = empty_ws_app(tmp.path());
        herdr_down(&mut a, tmp.path());
        let t = text(&draw(&a, 80, 14));
        assert!(t.contains("No PRs in this workspace"), "{t}");
        assert!(
            t.contains("Agent panes unknown while herdr isn't responding."),
            "{t}"
        );
        assert!(!t.contains("No agent panes"), "{t}");
    }

    #[test]
    fn ws_error_before_any_data_44() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = no_pr_workspace(tmp.path());
        a.apply_fetch(crate::fetch::FetchEvent::Started, now());
        fetch_error(&mut a, crate::github::GhError::LoginExpired);
        spin(&mut a, 2);
        let buf = draw(&a, 44, 14);
        let t = text(&buf);
        assert!(t.contains("✗ GitHub login expired."), "{t}");
        assert!(t.contains("   ✗ No PR data yet.  Press r to retry."), "{t}");
        assert!(!t.contains("looking up PRs"), "{t}");
        assert!(!t.contains('░'), "{t}");
        assert!(!t.contains('⠹'), "no spinner anywhere:\n{t}");
        insta::assert_snapshot!(t);
    }

    #[test]
    fn no_spinner_without_a_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w4:p1".into()); // not in a checkout
        a.apply_snapshot(
            crate::herdr::parse_snapshot(include_str!("../../tests/fixtures/snapshot.json")),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        for width in [80, 44] {
            let header = text_lines(&draw(&a, width, 14))[0].clone();
            assert!(!header.contains("loading"), "{width}: {header}");
        }
    }

    #[test]
    fn herdr_down_ws_44() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app_in(tmp.path());
        herdr_down(&mut a, tmp.path());
        let buf = draw(&a, 44, 30);
        let lines = text_lines(&buf);
        assert!(
            lines[1].starts_with(" ✗ herdr not responding · agent state ?"),
            "{}",
            lines[1]
        );
        assert_eq!(buf[(0, 1)].bg, Color::Red);
        assert!(
            lines
                .iter()
                .any(|l| l == "   ? = agent state unknown; ⏎ disabled")
        );
        insta::assert_snapshot!(text(&buf));
        a.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(a.screen(now()).status.as_deref(), Some("herdr unreachable"));
    }

    /// Spec §8: the agent dot shows once per stack, so while herdr is down the `?` takes
    /// the dot's place on the stack header only (which layer is checked out is unknown).
    #[test]
    fn herdr_down_marks_each_stack_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = ws_app_in(tmp.path());
        herdr_down(&mut a, tmp.path());
        for width in [80, 44, 34] {
            let lines = text_lines(&draw(&a, width, 30));
            let rows: Vec<&String> = lines.iter().filter(|l| l.contains(" #1")).collect();
            assert!(!rows.is_empty(), "{width}");
            assert!(rows.iter().all(|l| !l.contains(" ?")), "{width}: {rows:#?}");
            let headers = lines
                .iter()
                .filter(|l| l.starts_with("▾ ") && l.contains(" ?"));
            assert_eq!(headers.count(), 2, "{width}: {lines:#?}");
        }
    }

    #[test]
    fn workspace_herdr_down_at_startup_explains() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w1:p2".into());
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        let t = text(&draw(&a, 44, 14));
        assert!(t.contains("✗ herdr not responding"), "{t}");
        assert!(t.contains("until herdr answers"), "{t}");
    }

    #[test]
    fn workspace_outside_a_checkout_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w4:p1".into()); // /home/me: no git checkout
        a.apply_snapshot(
            crate::herdr::parse_snapshot(include_str!("../../tests/fixtures/snapshot.json")),
            &mut crate::git::FixtureBranches::embedded(),
            &StateDir::new(tmp.path().to_path_buf()),
            now(),
        );
        for width in [80, 44] {
            let t = text(&draw(&a, width, 14));
            assert!(
                t.contains("PR Pulse isn't in a git checkout"),
                "{width}: {t}"
            );
        }
    }

    #[test]
    fn filter_with_no_match_says_so() {
        let mut a = app();
        a.handle_key(key('/'));
        for c in "zzz".chars() {
            a.handle_key(key(c));
        }
        assert!(text(&draw(&a, 80, 20)).contains("no stacks match"));
    }

    // ------------------------------------------------------------ board 6: ✦ marks

    /// Board 6 option A: away since 14:18, back (visible) at 14:32:26, cursor on #1295.
    fn changed_app(kind: ViewKind, dir: &std::path::Path) -> App {
        let state = StateDir::new(dir.to_path_buf());
        crate::seed_board6(&state).unwrap();
        let a = match kind {
            ViewKind::Mine => app_with(|_| {}),
            ViewKind::Workspace => ws_app_in(dir),
        };
        let mut a = a.with_state(state);
        // A fresh visit against the seeded seen times.
        a.set_visible(false, now());
        a.set_visible(true, now());
        a.move_cursor(-100);
        a.move_cursor(4);
        a
    }

    #[test]
    fn changed_mine_120() {
        let tmp = tempfile::tempdir().unwrap();
        let a = changed_app(ViewKind::Mine, tmp.path());
        insta::assert_snapshot!(text(&draw(&a, 120, 30)));
    }

    #[test]
    fn changed_ws_44() {
        let tmp = tempfile::tempdir().unwrap();
        let a = changed_app(ViewKind::Workspace, tmp.path());
        insta::assert_snapshot!(text(&draw(&a, 44, 30)));
    }

    #[test]
    fn drawer_changed_120_on_1287() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = changed_app(ViewKind::Mine, tmp.path());
        a.move_cursor(-2);
        a.handle_key(key(' '));
        insta::assert_snapshot!(text(&draw(&a, 120, 36)));
    }

    #[test]
    fn changed_marks_are_bold_glyphs_and_cursor_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = changed_app(ViewKind::Mine, tmp.path());
        let buf = draw(&a, 120, 30);
        let y = line_of(&buf, "#1284");
        assert_eq!(buf[(0, y as u16)].symbol(), "✦");
        assert!(buf[(0, y as u16)].modifier.contains(Modifier::BOLD));
        // The # and title are bold too.
        let col = (0..120)
            .find(|&c| buf[(c, y as u16)].symbol() == "#")
            .unwrap();
        assert!(buf[(col, y as u16)].modifier.contains(Modifier::BOLD));
        // Cursor on a changed row: ▶ takes the gutter.
        a.move_cursor(-3); // #1284
        let buf = draw(&a, 120, 30);
        let y = line_of(&buf, "#1284");
        assert_eq!(buf[(0, y as u16)].symbol(), "▶");
        // Unchanged rows have neither.
        let y = line_of(&buf, "#415");
        assert_eq!(buf[(0, y as u16)].symbol(), " ");
    }

    #[test]
    fn changed_summary_by_width() {
        let tmp = tempfile::tempdir().unwrap();
        let a = changed_app(ViewKind::Mine, tmp.path());
        let summary = |w| text_lines(&draw(&a, w, 30))[1].clone();
        assert!(
            summary(120).ends_with("✦ 4 changed since you looked 14m ago"),
            "{}",
            summary(120)
        );
        assert!(summary(80).contains("✦"), "{}", summary(80));
        assert!(summary(44).ends_with("✦4 new"), "{}", summary(44));
        assert!(summary(34).ends_with("✦4 new"), "{}", summary(34));
        // Nothing marked: no suffix.
        assert!(!text_lines(&draw(&app(), 120, 30))[1].contains('✦'));
    }

    #[test]
    fn stale_rows_keep_the_mark_dim() {
        use crate::fetch::FetchEvent;
        let tmp = tempfile::tempdir().unwrap();
        let mut a = changed_app(ViewKind::Mine, tmp.path());
        let cache = crate::store::Cache {
            error: Some(crate::github::GhError::LoginExpired),
            ..Default::default()
        };
        a.apply_fetch(FetchEvent::Done(cache), now());
        let buf = draw(&a, 120, 30);
        let y = line_of(&buf, "#1284");
        assert_eq!(buf[(0, y as u16)].symbol(), "✦");
        assert_eq!(buf[(0, y as u16)].fg, Color::DarkGray);
    }
}
