//! Telemetry badges for sidebar cards: model, remaining context, session
//! cost, and rate-limit, formatted from the statusline-fed snapshot.
//!
//! Pure formatting over [`roster_core::Telemetry`] — absent readings render
//! nothing, so a pane without the statusline bridge contributes an empty
//! line. The sidebar draws it as a card's third line: the full badge line
//! on the focused card, and only the escalated context badge elsewhere —
//! at a glance the only telemetry with attention value is an agent about
//! to run out of context. See `docs/05-claude-native-attention.md`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use roster_core::{context_alert, rate_limit_alert, AgentState, ContextAlert, Telemetry};

use crate::sidebar::format_age;
use crate::style::{muted, normal, selected, selected_muted, state_color, WARN_ON_SELECTED};

/// Whether a card earns its telemetry row: always on the focused card, and
/// on any card whose context alert escalated — the one reading that must
/// interrupt a glance. The sidebar's row plan and this crate's rendering
/// both key off this, so a planned row can't render blank.
pub fn telemetry_row_visible(telemetry: &Telemetry, focused: bool) -> bool {
    focused || context_alert(telemetry.context_pct).is_some()
}

/// The `N% context` badge, in its severity color. `None` when the feed
/// never reported context. The sidebar draws this alone on an unfocused
/// card's telemetry row — a row `telemetry_row_visible` only plans when
/// the alert escalated, so the badge is present whenever such a row is.
/// `on_selected` re-keys the colors for the light selected surface.
pub(crate) fn context_badge(telemetry: &Telemetry, on_selected: bool) -> Option<Span<'static>> {
    let pct = telemetry.context_pct?;
    Some(Span::styled(
        format!("{pct:.0}% context"),
        context_style(telemetry.context_pct, on_selected),
    ))
}

/// The full telemetry badge line for the focused card: model, `N% context`,
/// `$X.XX`, and `limit N%` in that order, joined by muted `·` separators.
/// Absent readings render nothing — an unreported [`Telemetry`] yields an
/// empty line. The context badge carries the severity color (see
/// [`context_style`]); every other badge is quiet chrome.
///
/// The model's parenthetical variant suffix is dropped — Claude Code reports
/// display names like `Opus 4.8 (1M context)`, and on a ~30-column card the
/// suffix starves the numbers the badge exists to show.
///
/// `on_selected` builds the line for the light selected surface — the
/// focused card is the only place the full line renders, and it is always
/// the inverted one. This module owns the re-key so the severity vocabulary
/// can't be second-guessed from resolved colors at the call site.
pub fn telemetry_line(telemetry: &Telemetry, on_selected: bool) -> Line<'static> {
    let quiet = if on_selected {
        selected_muted()
    } else {
        muted()
    };
    let mut badges: Vec<Span<'static>> = Vec::new();
    if let Some(model) = &telemetry.model {
        let name = match model.find(" (") {
            Some(paren) => &model[..paren],
            None => model.as_str(),
        };
        badges.push(Span::styled(name.to_string(), quiet));
    }
    if let Some(badge) = context_badge(telemetry, on_selected) {
        badges.push(badge);
    }
    if let Some(cost) = telemetry.cost_usd {
        badges.push(Span::styled(format!("${cost:.2}"), quiet));
    }
    // Of the reported windows, badge the most-used one — a nearly spent
    // seven-day limit must not hide behind a fresh five-hour window.
    if let Some(window) =
        telemetry
            .rate_limit
            .as_ref()
            .and_then(|rate| match (&rate.five_hour, &rate.seven_day) {
                (Some(five), Some(seven)) if seven.used_pct > five.used_pct => Some(seven),
                (Some(five), _) => Some(five),
                (None, seven) => seven.as_ref(),
            })
    {
        let used = window.used_pct;
        let text = match window.resets_in {
            Some(resets) => format!("limit {used:.0}% resets {}", format_age(resets)),
            None => format!("limit {used:.0}%"),
        };
        badges.push(Span::styled(text, quiet));
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(badges.len() * 2);
    for badge in badges {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", quiet));
        }
        spans.push(badge);
    }
    Line::from(spans)
}

/// The context badge's style for a remaining-context reading: muted while
/// healthy, escalating through the same color vocabulary as the state dots —
/// the working yellow says look soon, the blocked red (bold) says the agent
/// is about to compact and lose the thread. Thresholds live in
/// [`roster_core::context_alert`], never here.
///
/// On the selected surface the critical red holds (it clears the light
/// fill — danger stays red everywhere), while the warn tier swaps the
/// unreadable-on-light yellow for the fixed dark amber
/// [`WARN_ON_SELECTED`].
fn context_style(remaining_pct: Option<f32>, on_selected: bool) -> Style {
    match (context_alert(remaining_pct), on_selected) {
        (None, false) => muted(),
        (None, true) => selected_muted(),
        (Some(ContextAlert::Warn), false) => Style::default().fg(state_color(AgentState::Working)),
        (Some(ContextAlert::Warn), true) => selected().fg(WARN_ON_SELECTED),
        (Some(ContextAlert::Critical), on_selected) => {
            let mut style = Style::default()
                .fg(state_color(AgentState::Blocked))
                .add_modifier(Modifier::BOLD);
            if on_selected {
                style = style.bg(selected().bg.expect("selected() always sets a fill"));
            }
            style
        }
    }
}

/// The severity style for a rate-limit window's used share on the dark
/// surfaces: the normal tier while healthy, escalating through the same
/// color vocabulary as the context badge — the working yellow from 70%
/// used, the blocked red (bold) from 90%. Thresholds live in
/// [`roster_core::rate_limit_alert`], never here. No selected-surface
/// variant: the fleet footer draws on the panel's base canvas only.
pub(crate) fn limit_style(used_pct: f32) -> Style {
    match rate_limit_alert(used_pct) {
        None => normal(),
        Some(ContextAlert::Warn) => Style::default().fg(state_color(AgentState::Working)),
        Some(ContextAlert::Critical) => Style::default()
            .fg(state_color(AgentState::Blocked))
            .add_modifier(Modifier::BOLD),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use roster_core::{RateLimit, RateLimitWindow};

    /// A telemetry with every field reported.
    fn full_telemetry() -> Telemetry {
        Telemetry {
            model: Some("claude-opus-4-8".to_string()),
            context_pct: Some(62.0),
            cost_usd: Some(1.23),
            rate_limit: Some(RateLimit {
                five_hour: Some(RateLimitWindow {
                    used_pct: 40.0,
                    resets_in: Some(Duration::from_secs(1800)),
                }),
                seven_day: Some(RateLimitWindow {
                    used_pct: 12.0,
                    resets_in: Some(Duration::from_secs(86_400)),
                }),
            }),
        }
    }

    /// Draw the full badge line for `telemetry` on a one-row TestBackend.
    fn draw(telemetry: &Telemetry, width: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        let line = telemetry_line(telemetry, false);
        terminal
            .draw(|frame| frame.render_widget(line, frame.area()))
            .unwrap();
        terminal
    }

    /// The drawn row as one string, one char per cell, trailing blanks
    /// trimmed.
    fn row_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area().width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The column where `needle` starts on the drawn row. Columns are char
    /// positions, not byte offsets — the `·` separator is multi-byte.
    fn col_of(terminal: &Terminal<TestBackend>, needle: &str) -> u16 {
        let row = row_text(terminal);
        let byte = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not on row: {row:?}"));
        row[..byte].chars().count() as u16
    }

    fn style_at(terminal: &Terminal<TestBackend>, x: u16) -> Style {
        terminal.backend().buffer().cell((x, 0)).unwrap().style()
    }

    #[test]
    fn all_badges_render_in_order_with_muted_separators() {
        let terminal = draw(&full_telemetry(), 70);
        assert_eq!(
            row_text(&terminal),
            "claude-opus-4-8 · 62% context · $1.23 · limit 40% resets 30m"
        );
        // Model, cost, rate-limit, and the separators are quiet chrome.
        for needle in ["claude-opus-4-8", "·", "$1.23", "limit 40%"] {
            let style = style_at(&terminal, col_of(&terminal, needle));
            assert_eq!(style.fg, muted().fg, "badge {needle:?} is not muted");
        }
    }

    #[test]
    fn model_parenthetical_suffix_is_dropped_from_the_badge() {
        // Live Claude Code reports "Opus 4.8 (1M context)" — on a ~30-column
        // card the suffix would starve the numbers.
        let telemetry = Telemetry {
            model: Some("Opus 4.8 (1M context)".to_string()),
            context_pct: Some(96.0),
            cost_usd: Some(0.02),
            ..Telemetry::default()
        };
        let terminal = draw(&telemetry, 40);
        assert_eq!(row_text(&terminal), "Opus 4.8 · 96% context · $0.02");
    }

    #[test]
    fn healthy_context_badge_stays_muted() {
        let telemetry = Telemetry {
            context_pct: Some(80.0),
            ..Telemetry::default()
        };
        let terminal = draw(&telemetry, 40);
        assert_eq!(row_text(&terminal), "80% context");
        let style = style_at(&terminal, col_of(&terminal, "80% context"));
        assert_eq!(style.fg, muted().fg);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn low_context_badge_takes_the_working_color() {
        let telemetry = Telemetry {
            context_pct: Some(20.0),
            ..Telemetry::default()
        };
        let terminal = draw(&telemetry, 40);
        let style = style_at(&terminal, col_of(&terminal, "20% context"));
        assert_eq!(style.fg, Some(state_color(AgentState::Working)));
    }

    #[test]
    fn critical_context_badge_takes_the_blocked_color_and_bold() {
        let telemetry = Telemetry {
            context_pct: Some(5.0),
            ..Telemetry::default()
        };
        let terminal = draw(&telemetry, 40);
        let style = style_at(&terminal, col_of(&terminal, "5% context"));
        assert_eq!(style.fg, Some(state_color(AgentState::Blocked)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn absent_readings_render_nothing() {
        // No readings at all: an empty line, a blank row.
        assert_eq!(row_text(&draw(&Telemetry::default(), 40)), "");
        // One reading: just that badge, no stray separators.
        let cost_only = Telemetry {
            cost_usd: Some(0.42),
            ..Telemetry::default()
        };
        assert_eq!(row_text(&draw(&cost_only, 40)), "$0.42");
        // A rate limit without a reset time renders only the used share.
        let rate_only = Telemetry {
            rate_limit: Some(RateLimit {
                five_hour: Some(RateLimitWindow {
                    used_pct: 40.0,
                    resets_in: None,
                }),
                seven_day: None,
            }),
            ..Telemetry::default()
        };
        assert_eq!(row_text(&draw(&rate_only, 40)), "limit 40%");
    }

    #[test]
    fn most_used_window_wins_the_rate_limit_badge() {
        // Seven-day nearly spent, five-hour fresh: the badge must show the
        // seven-day reading.
        let telemetry = Telemetry {
            rate_limit: Some(RateLimit {
                five_hour: Some(RateLimitWindow {
                    used_pct: 10.0,
                    resets_in: None,
                }),
                seven_day: Some(RateLimitWindow {
                    used_pct: 95.0,
                    resets_in: None,
                }),
            }),
            ..Telemetry::default()
        };
        assert_eq!(row_text(&draw(&telemetry, 40)), "limit 95%");
        // A seven-day-only report still gets a badge.
        let seven_only = Telemetry {
            rate_limit: Some(RateLimit {
                five_hour: None,
                seven_day: Some(RateLimitWindow {
                    used_pct: 60.0,
                    resets_in: None,
                }),
            }),
            ..Telemetry::default()
        };
        assert_eq!(row_text(&draw(&seven_only, 40)), "limit 60%");
    }

    #[test]
    fn context_badge_carries_the_severity_color_and_skips_absent_readings() {
        // No context reading: no badge — an unfocused card's telemetry row
        // draws this alone, so absence must render nothing.
        assert!(context_badge(&Telemetry::default(), false).is_none());
        let mut telemetry = full_telemetry();
        telemetry.context_pct = Some(5.0);
        let badge = context_badge(&telemetry, false).expect("badge for a reading");
        assert_eq!(badge.content, "5% context");
        assert_eq!(badge.style.fg, Some(state_color(AgentState::Blocked)));
    }

    #[test]
    fn telemetry_row_shows_on_focus_or_escalation_only() {
        let healthy = full_telemetry();
        assert!(telemetry_row_visible(&healthy, true));
        assert!(!telemetry_row_visible(&healthy, false));
        let mut low = full_telemetry();
        low.context_pct = Some(20.0);
        assert!(telemetry_row_visible(&low, false));
    }

    #[test]
    fn on_selected_lines_rekey_quiet_badges_and_keep_the_severity_vocabulary() {
        // Quiet badges and separators take the selected surface's
        // dark-muted tier.
        let line = telemetry_line(&full_telemetry(), true);
        for span in &line.spans {
            if span.content.contains("Opus")
                || span.content.contains('$')
                || span.content.contains("limit")
                || span.content.contains('·')
            {
                assert_eq!(span.style.fg, selected_muted().fg, "{:?}", span.content);
                assert_eq!(span.style.bg, selected_muted().bg, "{:?}", span.content);
            }
        }
        // Warn swaps the on-dark yellow for the dark amber — still warm,
        // still not the critical red.
        let warn = context_badge(
            &Telemetry {
                context_pct: Some(20.0),
                ..Telemetry::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(warn.style.fg, Some(WARN_ON_SELECTED));
        // Critical stays the blocked red, bold — danger is red on every
        // surface — with the light fill pinned behind it.
        let critical = context_badge(
            &Telemetry {
                context_pct: Some(5.0),
                ..Telemetry::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(critical.style.fg, Some(state_color(AgentState::Blocked)));
        assert_eq!(critical.style.bg, selected().bg);
        assert!(critical.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn badges_never_use_the_dim_attribute() {
        // The regression this extends (see style.rs): roster chrome must
        // carry explicit colors, never the near-invisible `DIM` attribute.
        let mut telemetry = full_telemetry();
        telemetry.context_pct = Some(5.0);
        let terminal = draw(&telemetry, 70);
        let buf = terminal.backend().buffer();
        for x in 0..buf.area().width {
            let cell = buf.cell((x, 0)).unwrap();
            assert!(
                !cell.style().add_modifier.contains(Modifier::DIM),
                "cell {x} uses DIM"
            );
        }
    }
}
