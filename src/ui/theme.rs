//! The only place colors are chosen. ANSI 16 only: Red, Green, Yellow, Cyan, DarkGray,
//! White (only on the hot row), Reset, plus BOLD and REVERSED.

use crate::attention::{AgentState, Attention};
use ratatui::style::{Color, Modifier, Style};

/// The red `▲▲` row: the only style that uses White.
pub fn hot_row() -> Style {
    Style::new()
        .bg(Color::Red)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub fn red() -> Style {
    Style::new().fg(Color::Red)
}

pub fn green() -> Style {
    Style::new().fg(Color::Green)
}

pub fn yellow() -> Style {
    Style::new().fg(Color::Yellow)
}

pub fn cyan() -> Style {
    Style::new().fg(Color::Cyan)
}

pub fn red_bold() -> Style {
    red().add_modifier(Modifier::BOLD)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// Header bar.
pub fn bar() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

// The cursor row is `▶` plus `bold()` with no background: a background would hide dim text
// on some themes (recorded decision).

pub fn attention(a: Attention) -> (&'static str, Style) {
    match a {
        Attention::Urgent => ("▲▲", hot_row()),
        Attention::NeedsYou => ("▲", red_bold()),
        Attention::Nudge => ("△", yellow().add_modifier(Modifier::BOLD)),
        Attention::Ready => ("◆", green().add_modifier(Modifier::BOLD)),
        Attention::Waiting => ("◷", dim()),
    }
}

pub fn agent(state: AgentState) -> (&'static str, Style) {
    match state {
        AgentState::Working => ("●", green()),
        AgentState::Blocked => ("◉", yellow().add_modifier(Modifier::BOLD)),
        AgentState::Idle => ("○", dim()),
        AgentState::Unknown => ("?", dim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::{AgentState, Attention};
    use ratatui::style::Color;

    fn all_styles() -> Vec<(String, Style)> {
        let mut v = vec![
            ("hot_row".to_string(), hot_row()),
            ("dim".into(), dim()),
            ("red".into(), red()),
            ("green".into(), green()),
            ("yellow".into(), yellow()),
            ("cyan".into(), cyan()),
            ("bold".into(), bold()),
            ("bar".into(), bar()),
        ];
        for a in [
            Attention::Waiting,
            Attention::Ready,
            Attention::Nudge,
            Attention::NeedsYou,
            Attention::Urgent,
        ] {
            v.push((format!("attention({a:?})"), attention(a).1));
        }
        for s in [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Idle,
            AgentState::Unknown,
        ] {
            v.push((format!("agent({s:?})"), agent(s).1));
        }
        v
    }

    #[test]
    fn palette_is_ansi_16_only() {
        let allowed = [
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Cyan,
            Color::DarkGray,
            Color::White,
            Color::Reset,
        ];
        for (name, s) in all_styles() {
            for c in [s.fg, s.bg].into_iter().flatten() {
                assert!(allowed.contains(&c), "{name} uses {c:?}");
            }
        }
    }

    #[test]
    fn only_the_hot_row_uses_white() {
        for (name, s) in all_styles() {
            let white = s.fg == Some(Color::White) || s.bg == Some(Color::White);
            if white {
                assert_eq!(
                    s,
                    hot_row(),
                    "{name} uses White but is not the hot row style"
                );
            }
        }
        assert_eq!(hot_row().fg, Some(Color::White));
        assert_eq!(hot_row().bg, Some(Color::Red));
    }

    #[test]
    fn glyphs() {
        let g: Vec<_> = [
            Attention::Urgent,
            Attention::NeedsYou,
            Attention::Nudge,
            Attention::Ready,
            Attention::Waiting,
        ]
        .into_iter()
        .map(|a| attention(a).0)
        .collect();
        assert_eq!(g, ["▲▲", "▲", "△", "◆", "◷"]);
        let a: Vec<_> = [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Idle,
            AgentState::Unknown,
        ]
        .into_iter()
        .map(|s| agent(s).0)
        .collect();
        assert_eq!(a, ["●", "◉", "○", "?"]);
    }
}
