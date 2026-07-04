//! Mapping between roster's model types and ratatui styling.

use ratatui::style::{Color, Modifier, Style};
use roster_core::{AgentState, CellStyle};

/// The ratatui color for an agent state's dot and label.
pub fn state_color(state: AgentState) -> Color {
    match state {
        AgentState::Blocked => Color::Red,
        AgentState::Working => Color::Yellow,
        AgentState::Done => Color::Blue,
        AgentState::Idle => Color::Green,
    }
}

/// The sidebar label for an agent state.
pub fn state_label(state: AgentState) -> &'static str {
    match state {
        AgentState::Blocked => "blocked",
        AgentState::Working => "working",
        AgentState::Done => "done",
        AgentState::Idle => "idle",
    }
}

/// Convert a grid cell's style into a ratatui [`Style`].
pub fn cell_style(style: CellStyle) -> Style {
    let mut out = Style::default()
        .fg(color(style.fg))
        .bg(color(style.bg));
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        out = out.add_modifier(Modifier::DIM);
    }
    if style.italic {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if style.reverse {
        out = out.add_modifier(Modifier::REVERSED);
    }
    out
}

fn color(color: roster_core::Color) -> Color {
    match color {
        roster_core::Color::Default => Color::Reset,
        roster_core::Color::Ansi(n) | roster_core::Color::Indexed(n) => Color::Indexed(n),
        roster_core::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_map_to_modifiers() {
        let style = cell_style(CellStyle {
            bold: true,
            underline: true,
            ..CellStyle::default()
        });
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn colors_map_faithfully() {
        assert_eq!(
            cell_style(CellStyle {
                fg: roster_core::Color::Rgb(1, 2, 3),
                ..CellStyle::default()
            })
            .fg,
            Some(Color::Rgb(1, 2, 3))
        );
        assert_eq!(
            cell_style(CellStyle {
                fg: roster_core::Color::Ansi(4),
                ..CellStyle::default()
            })
            .fg,
            Some(Color::Indexed(4))
        );
        assert_eq!(cell_style(CellStyle::default()).fg, Some(Color::Reset));
    }

    #[test]
    fn each_state_has_a_distinct_color() {
        let colors = [
            state_color(AgentState::Blocked),
            state_color(AgentState::Working),
            state_color(AgentState::Done),
            state_color(AgentState::Idle),
        ];
        for (i, a) in colors.iter().enumerate() {
            for b in colors.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
