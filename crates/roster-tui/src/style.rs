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

/// roster's brand color — a calm lavender from the fixed 256-color cube
/// (index 141 ≈ `#af87ff`) — the accent for the launch wordmark and all
/// interactive chrome (focused pane title, launcher frame, selection
/// markers, the keys in keyboard hints). Deliberately *not* red: red is
/// reserved for the blocked state and destructive affordances ([`danger`]),
/// and an accent that shared it made every piece of chrome read as alarm.
/// A fixed cube index for the same theme-independence reason as [`MUTED`];
/// contrast ~7.7:1 on pure black, ~5.6:1 on [`SURFACE_RAISED`]. The four
/// semantic state colors above stay distinct traffic-light hues: the accent
/// is the brand, the dots are the signal.
pub const ACCENT: Color = Color::Indexed(141);

/// The surface roster's chrome sits on — the app canvas behind panels and
/// between cards. Surface levels are background fills from the fixed
/// 256-color grayscale ramp (index 233 ≈ `#121212`), theme-independent for
/// the same reason as [`MUTED`]: roster commits to dark terminals — no
/// theming, no OSC background queries — so the levels are tuned against
/// dark and only dark.
pub const SURFACE_BASE: Color = Color::Indexed(233);

/// A raised surface — one level up from [`SURFACE_BASE`], the fill of
/// things that should read as sitting *on* the canvas: agent cards, and
/// (with the panel chrome) dialogs. Index 235 ≈ `#262626`.
pub const SURFACE_RAISED: Color = Color::Indexed(235);

/// The selected surface's fill — a light gray (index 254 ≈ `#e4e4e4`) that
/// inverts the card it covers. Selection reads as the one light card among
/// dark ones; an edge marker alone is a detail, a flipped surface is a
/// state.
const SELECTED_BG: Color = Color::Indexed(254);

/// Primary text on the selected surface (index 235 ≈ `#262626`, ~11.9:1 on
/// [`SELECTED_BG`]) — the same gray as [`SURFACE_RAISED`], so the inverted
/// card is literally the card ramp flipped.
const SELECTED_FG: Color = Color::Indexed(235);

/// Secondary text on the selected surface (index 240 ≈ `#585858`, ~5.6:1
/// on [`SELECTED_BG`]) — the muted tier's dark twin, for ages and badges on
/// an inverted card.
const SELECTED_MUTED_FG: Color = Color::Indexed(240);

/// The style of the selected surface: light fill, dark text. Callers layer
/// modifiers (bold names, the pulsing done glyph) on top; secondary text on
/// the surface uses [`selected_muted`].
pub fn selected() -> Style {
    Style::default().bg(SELECTED_BG).fg(SELECTED_FG)
}

/// The muted tier of the selected surface (see [`selected`]).
pub fn selected_muted() -> Style {
    Style::default().bg(SELECTED_BG).fg(SELECTED_MUTED_FG)
}

/// The warn tier on the selected surface: the working yellow that carries
/// "look soon" on dark fills has almost no contrast on the light fill, so
/// escalation-but-not-critical drops to a dark amber from the fixed cube
/// (index 94 ≈ `#875f00`, ~4.5:1 on [`SELECTED_BG`]) — still warm, still
/// not the critical red.
pub(crate) const WARN_ON_SELECTED: Color = Color::Indexed(94);

/// The bright foreground tier — primary text: card names, dialog titles.
/// Index 255 ≈ `#eeeeee`, ~13:1 on [`SURFACE_RAISED`]. Fixed-ramp for the
/// same reason as [`MUTED`]; roster never leans on the terminal's default
/// foreground for its own chrome.
const BRIGHT: Color = Color::Indexed(255);

/// The normal foreground tier — body text that must stay readable but not
/// lead: the reason line on a card. Index 250 ≈ `#bcbcbc`, ~8.5:1 on
/// [`SURFACE_RAISED`]. Sits between [`BRIGHT`] and [`MUTED`].
const NORMAL: Color = Color::Indexed(250);

/// A [`Style`] for the bright foreground tier (see [`BRIGHT`]).
pub fn bright() -> Style {
    Style::default().fg(BRIGHT)
}

/// A [`Style`] for the normal foreground tier (see [`NORMAL`]).
pub fn normal() -> Style {
    Style::default().fg(NORMAL)
}

/// The muted foreground for roster's own secondary "chrome" — status-line
/// hints, header subtitles, sidebar ages, launcher hints, unfocused pane
/// titles, and the thin rules between regions. The quietest tier of the
/// three-step foreground ramp ([`BRIGHT`] / [`NORMAL`] / [`MUTED`]).
///
/// A fixed mid-gray from the 256-color grayscale ramp (index 245 ≈
/// `#8a8a8a`), deliberately *not* the `DIM`/faint attribute. Many terminals
/// on their default palette render `DIM` as a barely-visible gray with
/// almost no contrast against the background; 245 clears a legible floor on
/// every dark surface (~5.4:1 on [`SURFACE_BASE`], ~4.4:1 on
/// [`SURFACE_RAISED`]). The grayscale ramp is fixed independently of the
/// user's 16-color theme, so a themed palette can't sink it. It was 243
/// when roster still balanced light terminals too; the dark commitment
/// bought the two brighter steps.
///
/// Change the gray here, in one place, to retune all muted chrome. This is
/// for roster-drawn UI only — guest program output keeps its faithful `DIM`
/// mapping in [`cell_style`].
const MUTED: Color = Color::Indexed(245);

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
/// inside a reversed cell where another inversion would cancel out. On
/// the light selected surface (`on_selected`) the reversal trick has
/// nothing dark to swap in, so the pill pins both sides explicitly: a
/// dark pill at rest, the accent fill with dark text when armed. Every
/// sidebar and status-row chip routes through here so rest, hover, and
/// armed can't drift apart between those controls. The modal dialogs
/// (confirm, exited) keep their own padded-button treatment — default and
/// danger fills this accent-only helper doesn't carry.
pub fn chip(armed: bool, hovered: bool, on_selected: bool) -> Style {
    let mut style = if on_selected {
        if armed {
            // Dark text on the accent fill (~5.6:1); the accent as a pill
            // *background* is what "armed" looks like on a light card.
            Style::default()
                .fg(SELECTED_FG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SELECTED_BG).bg(SELECTED_FG)
        }
    } else if armed {
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        muted().add_modifier(Modifier::REVERSED)
    };
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

/// [`state_glyph_style`] for a glyph on the light selected surface. The
/// hues that clear the light fill keep it — blocked red (~4.6:1) and done
/// azure (~3.6:1) stay the signal, so the one card you're looking at never
/// hides a block — while the low-luminance-on-light hues (working yellow,
/// idle green) drop to the surface's dark text and let shape and motion
/// carry them. Pulse phases match [`state_glyph_style`] tick for tick, so
/// the same done pane flips in step everywhere it renders.
pub fn state_glyph_style_selected(state: AgentState, tick: u64) -> Style {
    let mut style = selected();
    if matches!(state, AgentState::Blocked | AgentState::Done) {
        style = style.fg(state_color(state));
    }
    style.add_modifier(state_glyph_style(state, tick).add_modifier)
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
        let rest = chip(false, false, false);
        assert_eq!(rest.fg, Some(MUTED));
        assert!(rest.add_modifier.contains(Modifier::REVERSED));
        assert!(!rest.add_modifier.contains(Modifier::BOLD));
        // Armed: the accent fills the pill, bold so it survives no-color
        // terminals.
        let armed = chip(true, false, false);
        assert_eq!(armed.fg, Some(ACCENT));
        assert!(armed.add_modifier.contains(Modifier::REVERSED));
        assert!(armed.add_modifier.contains(Modifier::BOLD));
        // Hover underlines — visible inside the reversed pill in both
        // states, where a second inversion would cancel to nothing.
        for armed in [false, true] {
            assert!(chip(armed, true, false)
                .add_modifier
                .contains(Modifier::UNDERLINED));
            assert!(!chip(armed, false, false)
                .add_modifier
                .contains(Modifier::UNDERLINED));
        }
        // Never DIM — same guarantee as the rest of the chrome.
        assert!(!chip(false, false, false)
            .add_modifier
            .contains(Modifier::DIM));
    }

    #[test]
    fn chips_on_the_selected_surface_pin_both_sides_explicitly() {
        // The light fill leaves the reversal trick nothing dark to swap
        // in, so both pill states set fg AND bg: a dark pill at rest…
        let rest = chip(false, false, true);
        assert_eq!(rest.fg, Some(SELECTED_BG));
        assert_eq!(rest.bg, Some(SELECTED_FG));
        assert!(!rest.add_modifier.contains(Modifier::REVERSED));
        // …and the accent as the pill's background when armed, dark text
        // on it, bold as everywhere else.
        let armed = chip(true, false, true);
        assert_eq!(armed.bg, Some(ACCENT));
        assert_eq!(armed.fg, Some(SELECTED_FG));
        assert!(armed.add_modifier.contains(Modifier::BOLD));
        // Hover stays the underline, and DIM stays banned.
        assert!(chip(false, true, true)
            .add_modifier
            .contains(Modifier::UNDERLINED));
        for style in [rest, armed] {
            assert!(!style.add_modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn selected_surface_glyphs_keep_only_the_hues_that_clear_the_light_fill() {
        // Blocked and done keep their state hues — a block must stay red
        // even on the card you're parked on — while working and idle drop
        // to the surface's dark text.
        assert_eq!(
            state_glyph_style_selected(AgentState::Blocked, 0).fg,
            Some(state_color(AgentState::Blocked))
        );
        assert_eq!(
            state_glyph_style_selected(AgentState::Done, 0).fg,
            Some(state_color(AgentState::Done))
        );
        for state in [AgentState::Working, AgentState::Idle] {
            let style = state_glyph_style_selected(state, 0);
            assert_eq!(style.fg, Some(SELECTED_FG));
            assert_eq!(style.bg, Some(SELECTED_BG));
        }
        // The done pulse flips in step with the dark-surface variant.
        for tick in [0, PULSE_TICKS, 2 * PULSE_TICKS] {
            assert_eq!(
                state_glyph_style_selected(AgentState::Done, tick).add_modifier,
                state_glyph_style(AgentState::Done, tick).add_modifier,
                "tick {tick}"
            );
        }
    }

    #[test]
    fn accent_is_a_fixed_cube_color_and_never_the_danger_red() {
        // The accent is brand + interactive chrome; red is exclusively the
        // blocked state and destructive affordances. Sharing a hue made all
        // of roster's chrome read as alarm.
        assert_ne!(ACCENT, danger());
        for state in [
            AgentState::Blocked,
            AgentState::Working,
            AgentState::Done,
            AgentState::Idle,
        ] {
            assert_ne!(ACCENT, state_color(state), "accent collides with {state:?}");
        }
        // A fixed cube index, theme-independent like the rest of the system.
        assert!(matches!(ACCENT, Color::Indexed(_)));
    }

    #[test]
    fn surfaces_are_fixed_ramp_fills_and_selected_inverts_the_card_ramp() {
        assert!(matches!(SURFACE_BASE, Color::Indexed(_)));
        assert!(matches!(SURFACE_RAISED, Color::Indexed(_)));
        assert_ne!(SURFACE_BASE, SURFACE_RAISED);
        // The selected surface carries both fill and text — a call site
        // that only picks up one of them would paint light-on-light.
        let sel = selected();
        assert_eq!(sel.bg, Some(SELECTED_BG));
        assert_eq!(sel.fg, Some(SELECTED_FG));
        // Dark text on the light fill: the raised card's gray, flipped.
        assert_eq!(sel.fg, Some(SURFACE_RAISED));
        let sub = selected_muted();
        assert_eq!(sub.bg, sel.bg, "both selected tiers share the fill");
        assert_ne!(sub.fg, sel.fg);
        // Never DIM — same guarantee as every other piece of chrome.
        for style in [sel, sub] {
            assert!(!style.add_modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn foreground_ramp_has_three_distinct_explicit_tiers() {
        let tiers = [bright(), normal(), muted()];
        for tier in &tiers {
            assert!(tier.fg.is_some(), "every tier pins an explicit color");
            assert!(!tier.add_modifier.contains(Modifier::DIM));
            assert!(matches!(tier.fg, Some(Color::Indexed(_))));
        }
        for (i, a) in tiers.iter().enumerate() {
            for b in tiers.iter().skip(i + 1) {
                assert_ne!(a.fg, b.fg);
            }
        }
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
