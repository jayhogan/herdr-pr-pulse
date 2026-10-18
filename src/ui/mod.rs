mod cells;
mod chrome;
mod drawer;
pub mod help;
pub mod rows;
pub mod theme;

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::text::Span;

    use super::rows;
    use crate::app::{App, ViewKind};
    use crate::model;
    use crate::store::StateDir;

    fn now() -> DateTime<Utc> {
        "2026-10-17T14:32:26Z".parse().unwrap()
    }

    fn app_with(edit: impl FnOnce(&mut Vec<model::Pr>)) -> App {
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
            "2026-10-17T14:32:08Z".parse().unwrap(),
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
            assert!(lines[0].contains("✗ stale"), "{width}: {}", lines[0]);
            assert!(text(&buf).contains("✗ GitHub login expired"), "{width}");
            assert!(text(&buf).contains("#1287"), "{width}: rows kept");
        }
    }

    #[test]
    fn loading_lists_repos() {
        use crate::fetch::{FetchEvent, RepoStatus};
        let mut a = App::new(ViewKind::Mine, FixedOffset::east_opt(0).unwrap());
        assert!(text(&draw(&a, 80, 10)).contains("⠹ loading…"));
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
}
