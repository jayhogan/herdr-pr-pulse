pub mod rows;
pub mod theme;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, FixedOffset, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::text::Span;

    use super::rows;
    use crate::app::{App, Body, ViewKind};
    use crate::attention::AgentState;
    use crate::model;

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
        let Body::Groups(gs) = a.screen(now()).body else {
            panic!("expected groups")
        };
        let audit = gs.iter().find(|g| g.name == "audit").unwrap().key.clone();
        a.set_agents_for_test(BTreeMap::from([(audit, AgentState::Idle)]));
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
            .draw(|f| rows::render(f, &screen, &mut scroll))
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
    fn mine_120() {
        insta::assert_snapshot!(text(&draw(&app(), 120, 30)));
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
    fn density_breakpoints() {
        use rows::Density::*;
        let d: Vec<_> = [120, 100, 99, 60, 59, 40, 39, 1]
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
}
