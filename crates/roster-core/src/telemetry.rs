//! Agent telemetry snapshot shared by detection, ranking, and the sidebar.
//!
//! One bounded vocabulary for the statusline-fed numbers, so `roster-detect`
//! and `roster-tui` agree on shape without depending on each other. All
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

/// Rate-limit status reported by the agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimit {
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
                used_pct: 40.0,
                resets_in: Some(Duration::from_secs(1800)),
            }),
        };
        assert_eq!(t.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(t.context_pct, Some(62.5));
        assert_eq!(t.cost_usd, Some(1.23));
        let rl = t.rate_limit.expect("rate limit was set");
        assert_eq!(rl.used_pct, 40.0);
        assert_eq!(rl.resets_in, Some(Duration::from_secs(1800)));
    }
}
