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

/// roster's brand color — a dark red (rgb `223, 44, 44` / `#DF2C2C`) — used
/// as the accent for the launch wordmark and all interactive chrome (focused
/// pane title, launcher frame, selection markers). The four semantic state
/// colors above stay distinct traffic-light hues: the accent is the brand,
/// the dots are the signal.
pub const ACCENT: Color = Color::Rgb(223, 44, 44);

/// The muted foreground for roster's own secondary "chrome" — status-line
/// hints, header subtitles, sidebar ages and reasons, launcher hints,
/// unfocused pane titles, and the thin rules between regions.
///
/// A fixed mid-gray from the 256-color grayscale ramp (index 243 ≈ `#767676`),
/// deliberately *not* the `DIM`/faint attribute. Many terminals on their
/// default palette render `DIM` as a barely-visible gray with almost no
/// contrast against the background; 243 sits at the luminance balance point,
/// clearing a legible contrast floor (~4.6:1) against both pure black and pure
/// white. The grayscale ramp is fixed independently of the user's 16-color
/// theme, so this holds on both default-dark and default-light terminals.
///
/// Change the gray here, in one place, to retune all muted chrome. This is for
/// roster-drawn UI only — guest program output keeps its faithful `DIM`
/// mapping in [`cell_style`].
const MUTED: Color = Color::Indexed(243);

/// A [`Style`] for roster's muted secondary chrome (see [`MUTED`]). Call sites
/// layer their own modifiers (italic headers, reversed hover) on top.
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Spinner frames for the working state, advanced by the frame tick.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The status glyph for an agent state: a ringed dot demands attention
/// (blocked), a live spinner shows motion (working — animated by `tick`),
/// a check marks a finish (done), a hollow ring reads as at-rest (idle).
pub fn state_glyph(state: AgentState, tick: u64) -> &'static str {
    match state {
        AgentState::Blocked => "◉",
        AgentState::Working => SPINNER[(tick as usize) % SPINNER.len()],
        AgentState::Done => "✓",
        AgentState::Idle => "○",
    }
}

/// Convert a grid cell's style into a ratatui [`Style`].
pub fn cell_style(style: CellStyle) -> Style {
    let mut out = Style::default().fg(color(style.fg)).bg(color(style.bg));
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
    fn glyphs_are_distinct_and_working_animates() {
        assert_eq!(state_glyph(AgentState::Done, 0), "✓");
        assert_eq!(state_glyph(AgentState::Idle, 0), "○");
        assert_eq!(state_glyph(AgentState::Blocked, 0), "◉");
        // The working glyph cycles with the tick.
        assert_ne!(
            state_glyph(AgentState::Working, 0),
            state_glyph(AgentState::Working, 1)
        );
        assert_eq!(
            state_glyph(AgentState::Working, 0),
            state_glyph(AgentState::Working, 10)
        );
    }

    #[test]
    fn muted_chrome_is_an_explicit_color_not_the_dim_attribute() {
        // The bug this guards: chrome styled with the raw `DIM` attribute and
        // no explicit foreground renders as a near-invisible faint gray on a
        // terminal's default palette. Muted chrome must instead carry an
        // explicit foreground and must not lean on `DIM`.
        let style = muted();
        assert_eq!(style.fg, Some(MUTED));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        // A fixed grayscale-ramp index, so it's independent of the user's
        // 16-color theme (unlike ANSI "bright black").
        assert!(matches!(MUTED, Color::Indexed(_)));
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
