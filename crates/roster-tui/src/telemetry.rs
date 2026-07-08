//! Telemetry badges for sidebar cards: model, remaining context, session
//! cost, and rate-limit, formatted from the statusline-fed snapshot.
//!
//! Pure formatting over [`roster_core::Telemetry`] — absent readings render
//! nothing, so a pane without the statusline bridge contributes an empty
//! line. The sidebar draws it as a card's third line, only when the entry
//! carries telemetry. See `docs/05-claude-native-attention.md`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use roster_core::{context_alert, AgentState, ContextAlert, Telemetry};

use crate::sidebar::format_age;
use crate::style::{muted, state_color};

/// The telemetry badge line for one sidebar card: model, `N% context`,
/// `$X.XX`, and `limit N%` in that order, joined by muted `·` separators.
/// Absent readings render nothing — an unreported [`Telemetry`] yields an
/// empty line. The context badge carries the severity color (see
/// [`context_style`]); every other badge is quiet chrome.
pub fn telemetry_line(telemetry: &Telemetry) -> Line<'static> {
    let mut badges: Vec<Span<'static>> = Vec::new();
    if let Some(model) = &telemetry.model {
        badges.push(Span::styled(model.clone(), muted()));
    }
    if let Some(pct) = telemetry.context_pct {
        badges.push(Span::styled(
            format!("{pct:.0}% context"),
            context_style(telemetry.context_pct),
        ));
    }
    if let Some(cost) = telemetry.cost_usd {
        badges.push(Span::styled(format!("${cost:.2}"), muted()));
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
        badges.push(Span::styled(text, muted()));
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(badges.len() * 2);
    for badge in badges {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", muted()));
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
fn context_style(remaining_pct: Option<f32>) -> Style {
    match context_alert(remaining_pct) {
        None => muted(),
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

    /// Draw the badge line for `telemetry` on a one-row TestBackend.
    fn draw(telemetry: &Telemetry, width: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        let line = telemetry_line(telemetry);
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
