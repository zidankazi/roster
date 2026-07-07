//! Agent telemetry snapshot shared by detection, ranking, and the sidebar.
//!
//! One bounded vocabulary for the statusline-fed numbers, owned by the
//! zero-dep model crate so every consumer shares one shape. All
//! fields are optional: a pane without the statusline feed simply carries
//! `Telemetry::default()`. See `docs/05-claude-native-attention.md`.

/// A snapshot of the telemetry Claude Code reports via its statusline feed.
///
/// Every field is optional; absence means "not reported yet", never zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Telemetry {
    /// The model name as reported by the agent (e.g. `"claude-opus-4-8"`).
    pub model: Option<String>,
    /// Remaining context percentage (0–100) as provided by Claude Code —
    /// always the reported `remaining_percentage`, never computed locally.
    pub context_pct: Option<f32>,
    /// Session cost in US dollars as reported by the agent.
    pub cost_usd: Option<f32>,
    /// Rate-limit status, when the agent reports one.
    pub rate_limit: Option<RateLimit>,
}

/// Rate-limit status reported by the agent: one reading per window the
/// statusline feed documents. At least one window is present — an empty
/// report is `Telemetry::rate_limit == None`, never an all-`None` struct.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimit {
    /// The five-hour window, when reported.
    pub five_hour: Option<RateLimitWindow>,
    /// The seven-day window, when reported.
    pub seven_day: Option<RateLimitWindow>,
}

/// One rate-limit window's reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimitWindow {
    /// Percentage of the rate limit already used (0–100).
    pub used_pct: f32,
    /// Time until the limit resets, when the agent reports one.
    pub resets_in: Option<std::time::Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_telemetry_has_no_readings() {
        let t = Telemetry::default();
        assert_eq!(t.model, None);
        assert_eq!(t.context_pct, None);
        assert_eq!(t.cost_usd, None);
        assert_eq!(t.rate_limit, None);
    }

    #[test]
    fn populated_telemetry_keeps_every_field() {
        let t = Telemetry {
            model: Some("claude-opus-4-8".to_string()),
            context_pct: Some(62.5),
            cost_usd: Some(1.23),
            rate_limit: Some(RateLimit {
                five_hour: Some(RateLimitWindow {
                    used_pct: 40.0,
                    resets_in: Some(Duration::from_secs(1800)),
                }),
                seven_day: Some(RateLimitWindow {
                    used_pct: 75.5,
                    resets_in: Some(Duration::from_secs(86_400)),
                }),
            }),
        };
        assert_eq!(t.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(t.context_pct, Some(62.5));
        assert_eq!(t.cost_usd, Some(1.23));
        let rl = t.rate_limit.expect("rate limit was set");
        let five = rl.five_hour.expect("five-hour window was set");
        assert_eq!(five.used_pct, 40.0);
        assert_eq!(five.resets_in, Some(Duration::from_secs(1800)));
        let seven = rl.seven_day.expect("seven-day window was set");
        assert_eq!(seven.used_pct, 75.5);
        assert_eq!(seven.resets_in, Some(Duration::from_secs(86_400)));
    }
}
