//! Claude Code statusline payload → [`Telemetry`].
//!
//! Claude Code pipes a session JSON to the configured statusline command;
//! that feed is the sanctioned telemetry source — never the session
//! transcript (see `docs/05-claude-native-attention.md`). Parsing is
//! all-optional: a missing, `null`, or mistyped field yields `None` for
//! that field alone, and only input that is not a JSON object at all fails
//! the parse. Rate-limit `resets_at` arrives as unix epoch seconds and is
//! converted to a remaining duration, saturating to zero for past resets.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use roster_core::{RateLimit, RateLimitWindow, Telemetry};
use serde_json::Value;

/// The telemetry carried by a statusline JSON payload, or `None` when the
/// input is not a JSON object. Absent fields stay `None` — never an error.
pub fn parse(json: &str) -> Option<Telemetry> {
    let root: Value = serde_json::from_str(json).ok()?;
    if !root.is_object() {
        return None;
    }
    let now_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some(read_telemetry(&root, now_epoch_secs))
}

/// The field-by-field mapping, with the clock injected so the epoch math is
/// testable without a real `SystemTime`.
fn read_telemetry(root: &Value, now_epoch_secs: u64) -> Telemetry {
    Telemetry {
        model: root
            .get("model")
            .and_then(|m| m.get("display_name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        context_pct: num_at(root, "context_window", "remaining_percentage"),
        cost_usd: num_at(root, "cost", "total_cost_usd"),
        rate_limit: read_rate_limit(root, now_epoch_secs),
    }
}

/// The `root[outer][inner]` number as an `f32`, if present and numeric.
fn num_at(root: &Value, outer: &str, inner: &str) -> Option<f32> {
    Some(root.get(outer)?.get(inner)?.as_f64()? as f32)
}

/// The rate-limit report, if any window is present. The feed documents two
/// windows (`five_hour`, `seven_day`); each is read independently, and a
/// report with neither reads as no rate limit at all.
fn read_rate_limit(root: &Value, now_epoch_secs: u64) -> Option<RateLimit> {
    let limits = root.get("rate_limits")?;
    let five_hour = read_window(limits, "five_hour", now_epoch_secs);
    let seven_day = read_window(limits, "seven_day", now_epoch_secs);
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(RateLimit {
        five_hour,
        seven_day,
    })
}

/// One named window's reading. A reading requires `used_percentage`;
/// `resets_at` may be independently absent, and a reset already in the past
/// reads as a zero remaining duration.
fn read_window(limits: &Value, name: &str, now_epoch_secs: u64) -> Option<RateLimitWindow> {
    let window = limits.get(name)?;
    let used_pct = window.get("used_percentage")?.as_f64()? as f32;
    let resets_in = window
        .get("resets_at")
        .and_then(Value::as_u64)
        .map(|at| Duration::from_secs(at.saturating_sub(now_epoch_secs)));
    Some(RateLimitWindow {
        used_pct,
        resets_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(json: &str) -> Value {
        serde_json::from_str(json).expect("test json is valid")
    }

    #[test]
    fn future_reset_becomes_remaining_duration() {
        let v = root(r#"{"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1000}}}"#);
        let rl = read_telemetry(&v, 400).rate_limit.expect("window reported");
        let w = rl.five_hour.expect("five-hour window reported");
        assert_eq!(w.used_pct, 23.5);
        assert_eq!(w.resets_in, Some(Duration::from_secs(600)));
        assert_eq!(rl.seven_day, None);
    }

    #[test]
    fn past_reset_saturates_to_zero_duration() {
        let v = root(r#"{"rate_limits":{"five_hour":{"used_percentage":90,"resets_at":1000}}}"#);
        let rl = read_telemetry(&v, 2000)
            .rate_limit
            .expect("window reported");
        let w = rl.five_hour.expect("five-hour window reported");
        assert_eq!(w.resets_in, Some(Duration::ZERO));
    }

    #[test]
    fn both_windows_are_read_independently() {
        let v = root(
            r#"{"rate_limits":{
                "five_hour":{"used_percentage":23.5,"resets_at":1000},
                "seven_day":{"used_percentage":41.2}
            }}"#,
        );
        let rl = read_telemetry(&v, 400)
            .rate_limit
            .expect("windows reported");
        assert_eq!(rl.five_hour.expect("five-hour reported").used_pct, 23.5);
        let seven = rl.seven_day.expect("seven-day reported");
        assert_eq!(seven.used_pct, 41.2);
        assert_eq!(seven.resets_in, None);
    }

    #[test]
    fn seven_day_alone_still_reports_a_rate_limit() {
        let v = root(r#"{"rate_limits":{"seven_day":{"used_percentage":88.0}}}"#);
        let rl = read_telemetry(&v, 0).rate_limit.expect("window reported");
        assert_eq!(rl.five_hour, None);
        assert_eq!(rl.seven_day.expect("seven-day reported").used_pct, 88.0);
    }

    #[test]
    fn missing_used_percentage_drops_the_window() {
        let v = root(r#"{"rate_limits":{"five_hour":{"resets_at":1000}}}"#);
        assert_eq!(read_telemetry(&v, 0).rate_limit, None);
    }

    #[test]
    fn missing_resets_at_keeps_the_used_percentage() {
        let v = root(r#"{"rate_limits":{"five_hour":{"used_percentage":41.2}}}"#);
        let rl = read_telemetry(&v, 0).rate_limit.expect("window reported");
        let w = rl.five_hour.expect("five-hour window reported");
        assert_eq!(w.used_pct, 41.2);
        assert_eq!(w.resets_in, None);
    }

    #[test]
    fn null_remaining_percentage_reads_as_absent_not_zero() {
        let v = root(r#"{"context_window":{"remaining_percentage":null,"used_percentage":null}}"#);
        assert_eq!(read_telemetry(&v, 0).context_pct, None);
    }

    #[test]
    fn mistyped_field_is_absent_without_failing_the_rest() {
        let v = root(r#"{"model":{"display_name":42},"cost":{"total_cost_usd":0.5}}"#);
        let t = read_telemetry(&v, 0);
        assert_eq!(t.model, None);
        assert_eq!(t.cost_usd, Some(0.5));
    }
}
