//! Claude Code statusline payload → [`Payload`] (telemetry + session name).
//!
//! Claude Code pipes a session JSON to the configured statusline command;
//! that feed is the sanctioned telemetry source — never the session
//! transcript (see `docs/05-claude-native-attention.md`). Parsing is
//! all-optional: a missing, `null`, or mistyped field yields `None` for
//! that field alone; input that is not a JSON object — or an object where
//! nothing we map is present (payload keys drifted, or junk) — fails the
//! parse, so downstream never grows UI for an empty reading. Rate-limit
//! `resets_at` arrives as unix epoch seconds and is converted to a
//! remaining duration, saturating to zero for past resets.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use roster_core::{RateLimit, RateLimitWindow, Telemetry};
use serde_json::Value;

/// What one statusline payload reports, split by lifetime: the badge
/// numbers age out with the feed, while the session identity and name are
/// display state the pane keeps (see `Session::set_session_name`). Keeping
/// the name out of [`Telemetry`] is deliberate — one fact in one place, so
/// a consumer can't read the aging copy by accident.
#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    /// The badge readings, when the payload carried any — `None` keeps the
    /// no-blank-badge contract: a payload without numbers must not replace
    /// a pane's fresh reading or grow an empty badge row.
    pub telemetry: Option<Telemetry>,
    /// The reporting session's id — an opaque routing/identity token only,
    /// never a handle to read the transcript by.
    pub session_id: Option<String>,
    /// Claude Code's own name for the session (its auto-generated summary,
    /// e.g. `"Acknowledge request"`); absent until the session is first
    /// summarized.
    pub session_name: Option<String>,
}

/// The readings carried by a statusline JSON payload, or `None` when the
/// input is not a JSON object or nothing we map is present. Absent fields
/// stay `None` — never an error. The telemetry sub-reading is `None`
/// unless a badge field was reported — an all-empty reading is "no
/// telemetry", not a report of nothing, so a card never grows a blank
/// badge line for it.
pub fn parse(json: &str) -> Option<Payload> {
    let root: Value = serde_json::from_str(json).ok()?;
    if !root.is_object() {
        return None;
    }
    let now_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = Payload {
        telemetry: Some(read_telemetry(&root, now_epoch_secs))
            .filter(|telemetry| *telemetry != Telemetry::default()),
        session_id: string_at(&root, "session_id"),
        session_name: string_at(&root, "session_name"),
    };
    let empty = payload.telemetry.is_none()
        && payload.session_id.is_none()
        && payload.session_name.is_none();
    (!empty).then_some(payload)
}

/// The root-level string field `key`, if present and a string.
fn string_at(root: &Value, key: &str) -> Option<String> {
    root.get(key).and_then(Value::as_str).map(str::to_string)
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
    fn empty_or_unmapped_objects_parse_to_nothing() {
        assert_eq!(parse("{}"), None);
        assert_eq!(parse(r#"{"version":"2.1.0","fast_mode":false}"#), None);
        // One mapped field is a report.
        assert!(parse(r#"{"cost":{"total_cost_usd":0.5}}"#).is_some());
    }

    #[test]
    fn a_payload_without_numbers_carries_no_telemetry_reading() {
        // The session fields parse, but `telemetry` stays `None`: a
        // numbers-less payload must not replace a pane's fresh reading or
        // grow a blank badge row — the name is not a badge.
        let payload = parse(r#"{"session_id":"s","session_name":"Fix the auth flow"}"#)
            .expect("session fields are a report");
        assert_eq!(payload.telemetry, None);
        assert_eq!(payload.session_id.as_deref(), Some("s"));
        assert_eq!(payload.session_name.as_deref(), Some("Fix the auth flow"));
    }

    #[test]
    fn mistyped_field_is_absent_without_failing_the_rest() {
        let v = root(r#"{"model":{"display_name":42},"cost":{"total_cost_usd":0.5}}"#);
        let t = read_telemetry(&v, 0);
        assert_eq!(t.model, None);
        assert_eq!(t.cost_usd, Some(0.5));
    }
}
