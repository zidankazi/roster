//! Context-exhaustion alerting for the sidebar.
//!
//! Pure thresholding over the statusline-reported remaining-context
//! percentage (see [`crate::Telemetry::context_pct`]); no reading means no
//! alert, never a guess. See `docs/05-claude-native-attention.md`.

/// The urgency of a telemetry reading — the shared warn/critical severity
/// vocabulary for the remaining-context thresholds here and the rate-limit
/// thresholds in [`crate::rate_limit_alert`]. Ordered by severity:
/// `Warn < Critical`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextAlert {
    /// Context is getting low; worth a glance.
    Warn,
    /// Context is nearly exhausted; the agent is about to compact or stall.
    Critical,
}

/// Remaining-context percentage at or below which the alert is [`ContextAlert::Warn`].
pub const WARN_THRESHOLD_PCT: f32 = 25.0;
/// Remaining-context percentage at or below which the alert is [`ContextAlert::Critical`].
pub const CRITICAL_THRESHOLD_PCT: f32 = 10.0;

/// The alert level for a remaining-context percentage, or `None` when the
/// reading is absent or healthy.
///
/// A non-finite reading (NaN, ±inf) is garbage, not a measurement, and is
/// treated as absent rather than classified.
pub fn context_alert(remaining_pct: Option<f32>) -> Option<ContextAlert> {
    let pct = remaining_pct.filter(|p| p.is_finite())?;
    if pct <= CRITICAL_THRESHOLD_PCT {
        Some(ContextAlert::Critical)
    } else if pct <= WARN_THRESHOLD_PCT {
        Some(ContextAlert::Warn)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_input_no_alert() {
        assert_eq!(context_alert(None), None);
    }

    #[test]
    fn critical_below_ten() {
        assert_eq!(context_alert(Some(5.0)), Some(ContextAlert::Critical));
        assert_eq!(context_alert(Some(0.0)), Some(ContextAlert::Critical));
    }

    #[test]
    fn warn_between_ten_and_twentyfive() {
        assert_eq!(context_alert(Some(15.0)), Some(ContextAlert::Warn));
        assert_eq!(context_alert(Some(24.9)), Some(ContextAlert::Warn));
    }

    #[test]
    fn healthy_above_threshold_none() {
        assert_eq!(context_alert(Some(25.1)), None);
        assert_eq!(context_alert(Some(100.0)), None);
    }

    #[test]
    fn boundary_exactly_ten_is_critical() {
        assert_eq!(context_alert(Some(10.0)), Some(ContextAlert::Critical));
    }

    #[test]
    fn non_finite_readings_are_treated_as_absent() {
        assert_eq!(context_alert(Some(f32::NAN)), None);
        assert_eq!(context_alert(Some(f32::INFINITY)), None);
        assert_eq!(context_alert(Some(f32::NEG_INFINITY)), None);
    }

    #[test]
    fn boundary_exactly_twentyfive_is_warn() {
        assert_eq!(context_alert(Some(25.0)), Some(ContextAlert::Warn));
    }
}
