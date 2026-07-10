//! Mapping between roster's model types and ratatui styling.

use ratatui::style::{Color, Modifier, Style};
use roster_core::{AgentState, CellStyle};

/// The ratatui color for an agent state's dot and label.
pub fn state_color(state: AgentState) -> Color {
    match state {
        AgentState::Blocked => Color::Red,
        AgentState::Working => Color::Yellow,
        // Blue is intrinsically low-luminance, so the themeable ANSI blue
        // (index 4) renders as a near-invisible dark navy on a default-palette
        // dark terminal. Pin done to a fixed azure from the 256-color cube
        // (index 33, #0087ff) instead — legible on both dark (~5.9:1) and
        // light (~3.6:1). Red/yellow/green carry enough luminance to stay
        // readable as the adaptive named colors.
        AgentState::Done => Color::Indexed(33),
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

/// The red for destructive affordances and error chrome (close buttons,
/// error toasts) — the same red as the blocked state, routed through one
/// place so a retune of the palette can't strand hardcoded copies.
pub fn danger() -> Color {
    state_color(AgentState::Blocked)
}

/// The style for roster's clickable chrome — toggle chips and buttons,
/// drawn as space-padded reverse-video pills (` auto `), never bracketed
/// text. The filled pill is the affordance: it reads as pressable even in
/// terminals that ignore the pointer-shape protocol and show no hand
/// cursor. `armed` fills the pill with the accent (bold) for an active
/// toggle or mode; `hovered` underlines it — underline stays visible
/// inside a reversed cell where another inversion would cancel out. Every
/// chip routes through here so rest, hover, and armed can't drift apart
/// between controls.
pub fn chip(armed: bool, hovered: bool) -> Style {
    let mut style = if armed {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        muted()
    };
    style = style.add_modifier(Modifier::REVERSED);
    if hovered {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
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

/// Frame ticks per phase of the done pulse: at the ~125ms frame tick the
/// glyph flips every ~500ms — slow enough not to strobe, fast enough to
/// catch an eye sweeping the sidebar.
const PULSE_TICKS: u64 = 4;

/// The style for a state's status glyph, animated by the frame tick. Done
/// pulses — plain and reversed in alternating phases — because done means
/// an agent finished and is waiting for the human to look, and a steady
/// check among steady dots is exactly what a sweeping eye skips. Both
/// phases keep the same explicit foreground, so the glyph never dims or
/// disappears (see [`muted`] for why that matters). The other states
/// render steady: blocked already leads the triage order, and working's
/// motion is its spinner.
pub fn state_glyph_style(state: AgentState, tick: u64) -> Style {
    let base = Style::default().fg(state_color(state));
    if state == AgentState::Done && (tick / PULSE_TICKS) % 2 == 1 {
        base.add_modifier(Modifier::REVERSED)
    } else {
        base
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
    fn done_glyph_pulses_and_other_states_hold_steady() {
        let off = state_glyph_style(AgentState::Done, 0);
        let on = state_glyph_style(AgentState::Done, PULSE_TICKS);
        assert!(!off.add_modifier.contains(Modifier::REVERSED));
        assert!(on.add_modifier.contains(Modifier::REVERSED));
        // Both phases keep the explicit state color — the pulse must never
        // pass through a dimmed or foregroundless frame.
        assert_eq!(off.fg, Some(state_color(AgentState::Done)));
        assert_eq!(on.fg, Some(state_color(AgentState::Done)));
        // The cycle repeats rather than drifting.
        assert_eq!(state_glyph_style(AgentState::Done, 2 * PULSE_TICKS), off);
        for state in [AgentState::Blocked, AgentState::Working, AgentState::Idle] {
            assert_eq!(
                state_glyph_style(state, 0),
                state_glyph_style(state, PULSE_TICKS)
            );
        }
    }

    #[test]
    fn chips_are_reversed_pills_with_accent_when_armed_and_underline_on_hover() {
        // Rest: a quiet muted pill — the reversal is the button shape.
        let rest = chip(false, false);
        assert_eq!(rest.fg, Some(MUTED));
        assert!(rest.add_modifier.contains(Modifier::REVERSED));
        assert!(!rest.add_modifier.contains(Modifier::BOLD));
        // Armed: the accent fills the pill, bold so it survives no-color
        // terminals.
        let armed = chip(true, false);
        assert_eq!(armed.fg, Some(ACCENT));
        assert!(armed.add_modifier.contains(Modifier::REVERSED));
        assert!(armed.add_modifier.contains(Modifier::BOLD));
        // Hover underlines — visible inside the reversed pill in both
        // states, where a second inversion would cancel to nothing.
        for armed in [false, true] {
            assert!(chip(armed, true)
                .add_modifier
                .contains(Modifier::UNDERLINED));
            assert!(!chip(armed, false)
                .add_modifier
                .contains(Modifier::UNDERLINED));
        }
        // Never DIM — same guarantee as the rest of the chrome.
        assert!(!chip(false, false).add_modifier.contains(Modifier::DIM));
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
    fn done_uses_a_legible_explicit_blue_not_ansi_blue() {
        // ANSI blue (index 4) is near-invisible on a default-palette dark
        // terminal. Done must use an explicit cube color so it stays legible
        // regardless of the user's 16-color theme.
        let done = state_color(AgentState::Done);
        assert_ne!(done, Color::Blue);
        assert!(matches!(done, Color::Indexed(_)));
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
