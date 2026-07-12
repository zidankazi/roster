//! Agent telemetry snapshot shared by detection, ranking, and the sidebar.
//!
//! One bounded vocabulary for the statusline-fed numbers, owned by the
//! zero-dep model crate so every consumer shares one shape. All
//! fields are optional: a pane without the statusline feed simply carries
//! `Telemetry::default()`. Rate limits are account-scoped rather than
//! per-pane, so this module also owns the fleet view over them: the
//! freshest-wins aggregation ([`fleet_rate_limit`]), the used-share
//! thresholds ([`rate_limit_alert`]), and the edge-triggered crossing
//! detector behind the limit toasts ([`LimitNotifier`]) — all pure, so the
//! binary only wires effects. See `docs/05-claude-native-attention.md`.

use std::time::{Duration, Instant};

use crate::context::ContextAlert;

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

/// Used percentage at or above which a rate-limit window is
/// [`ContextAlert::Warn`].
pub const LIMIT_WARN_THRESHOLD_PCT: f32 = 70.0;
/// Used percentage at or above which a rate-limit window is
/// [`ContextAlert::Critical`].
pub const LIMIT_CRITICAL_THRESHOLD_PCT: f32 = 90.0;

/// The alert level for a rate-limit window's used percentage, or `None`
/// while healthy. The same severity vocabulary as [`crate::context_alert`],
/// keyed the opposite way: context alerts on what *remains*, limits alert
/// on what is *used*.
///
/// A non-finite reading (NaN, ±inf) is garbage, not a measurement, and is
/// treated as healthy rather than classified.
pub fn rate_limit_alert(used_pct: f32) -> Option<ContextAlert> {
    if !used_pct.is_finite() {
        return None;
    }
    if used_pct >= LIMIT_CRITICAL_THRESHOLD_PCT {
        Some(ContextAlert::Critical)
    } else if used_pct >= LIMIT_WARN_THRESHOLD_PCT {
        Some(ContextAlert::Warn)
    } else {
        None
    }
}

/// The account-wide rate-limit view across a fleet of panes' readings, each
/// stamped with when it arrived. Rate limits are account-scoped — every
/// live feed reports the same account — so per window the freshest reading
/// wins, and windows resolve independently: a pane whose payload carried
/// only the five-hour window must not erase another pane's seven-day
/// reading. `None` when no reading carries the window at all. Staleness is
/// the caller's contract: feed only readings still live under the per-card
/// aging rule, so the fleet view can never outlive the badges it mirrors.
pub fn fleet_rate_limit<'a, I>(readings: I) -> Option<RateLimit>
where
    I: IntoIterator<Item = (&'a RateLimit, Instant)>,
{
    let mut five_hour: Option<(&RateLimitWindow, Instant)> = None;
    let mut seven_day: Option<(&RateLimitWindow, Instant)> = None;
    // A non-finite used share is garbage, not a measurement (the feed's
    // f64→f32 cast can overflow to +inf): it must neither erase a finite
    // reading on freshness nor occupy a footer row as a healthy-looking
    // gauge — the same is-a-measurement bar `rate_limit_alert` holds.
    fn finite(window: &Option<RateLimitWindow>) -> Option<&RateLimitWindow> {
        window.as_ref().filter(|window| window.used_pct.is_finite())
    }
    for (limit, at) in readings {
        if let Some(window) = finite(&limit.five_hour) {
            if five_hour.is_none_or(|(_, seen)| at > seen) {
                five_hour = Some((window, at));
            }
        }
        if let Some(window) = finite(&limit.seven_day) {
            if seven_day.is_none_or(|(_, seen)| at > seen) {
                seven_day = Some((window, at));
            }
        }
    }
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(RateLimit {
        five_hour: five_hour.map(|(window, _)| window.clone()),
        seven_day: seven_day.map(|(window, _)| window.clone()),
    })
}

/// Which account rate-limit window a notice concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitWindow {
    /// The five-hour window.
    FiveHour,
    /// The seven-day window.
    SevenDay,
}

/// A rate-limit threshold crossing worth telling the human about.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitNotice {
    /// The window that crossed.
    pub window: LimitWindow,
    /// The tier crossed into: warn at 70% used, critical at 90%.
    pub alert: ContextAlert,
    /// The used percentage that crossed.
    pub used_pct: f32,
    /// Time until the window resets, when the reading carried one.
    pub resets_in: Option<Duration>,
}

/// Edge-triggered crossing detection over the fleet's rate-limit readings:
/// one notice per threshold per window, however long the readings sit above
/// it. Feed it every aggregated reading ([`fleet_rate_limit`]); it fires
/// only when a window's alert tier rises, and re-arms when the tier falls —
/// a window reset zeroes the used share, so "used % dropped below the
/// threshold" covers resets too. The state is pure; the binary owns turning
/// notices into toasts.
#[derive(Debug, Default)]
pub struct LimitNotifier {
    five_hour: Option<ContextAlert>,
    seven_day: Option<ContextAlert>,
}

impl LimitNotifier {
    /// A notifier with no thresholds crossed yet.
    pub fn new() -> Self {
        LimitNotifier::default()
    }

    /// Feed the current fleet reading; the notices that fire on it, loudest
    /// tier only — a reading that jumps straight past both thresholds is
    /// one critical notice, not a warn and a critical stacked.
    pub fn observe(&mut self, limits: Option<&RateLimit>) -> Vec<LimitNotice> {
        let mut notices = Vec::new();
        let five_hour = limits.and_then(|limit| limit.five_hour.as_ref());
        let seven_day = limits.and_then(|limit| limit.seven_day.as_ref());
        Self::observe_window(
            &mut self.five_hour,
            five_hour,
            LimitWindow::FiveHour,
            &mut notices,
        );
        Self::observe_window(
            &mut self.seven_day,
            seven_day,
            LimitWindow::SevenDay,
            &mut notices,
        );
        notices
    }

    /// One window's edge step: fire when the alert tier rises, silently
    /// re-arm when it falls. An absent reading (feed stale or gone — a
    /// non-finite used share included) leaves the edge state alone: a
    /// flapping feed sitting at 75% must not re-notify on every return,
    /// while a real reset reads as the used share falling, which re-arms.
    fn observe_window(
        held: &mut Option<ContextAlert>,
        reading: Option<&RateLimitWindow>,
        window: LimitWindow,
        notices: &mut Vec<LimitNotice>,
    ) {
        let Some(reading) = reading.filter(|reading| reading.used_pct.is_finite()) else {
            return;
        };
        let level = rate_limit_alert(reading.used_pct);
        if level > *held {
            notices.push(LimitNotice {
                window,
                alert: level.expect("a tier above the held one is a crossed tier"),
                used_pct: reading.used_pct,
                resets_in: reading.resets_in,
            });
        }
        *held = level;
    }
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

    /// A rate limit reporting only the five-hour window.
    fn five_only(used_pct: f32) -> RateLimit {
        RateLimit {
            five_hour: Some(RateLimitWindow {
                used_pct,
                resets_in: None,
            }),
            seven_day: None,
        }
    }

    #[test]
    fn limit_alert_escalates_at_seventy_and_ninety_used() {
        assert_eq!(rate_limit_alert(0.0), None);
        assert_eq!(rate_limit_alert(69.9), None);
        assert_eq!(rate_limit_alert(70.0), Some(ContextAlert::Warn));
        assert_eq!(rate_limit_alert(89.9), Some(ContextAlert::Warn));
        assert_eq!(rate_limit_alert(90.0), Some(ContextAlert::Critical));
        assert_eq!(rate_limit_alert(100.0), Some(ContextAlert::Critical));
    }

    #[test]
    fn non_finite_used_shares_read_as_healthy_not_classified() {
        assert_eq!(rate_limit_alert(f32::NAN), None);
        assert_eq!(rate_limit_alert(f32::INFINITY), None);
        assert_eq!(rate_limit_alert(f32::NEG_INFINITY), None);
    }

    #[test]
    fn freshest_reading_wins_each_fleet_window() {
        let now = Instant::now();
        let older = RateLimit {
            five_hour: Some(RateLimitWindow {
                used_pct: 40.0,
                resets_in: Some(Duration::from_secs(600)),
            }),
            seven_day: Some(RateLimitWindow {
                used_pct: 10.0,
                resets_in: None,
            }),
        };
        let newer = five_only(62.0);
        // Order in the iterator must not matter — only the stamps do.
        for readings in [
            vec![(&older, now), (&newer, now + Duration::from_secs(5))],
            vec![(&newer, now + Duration::from_secs(5)), (&older, now)],
        ] {
            let fleet = fleet_rate_limit(readings).expect("readings present");
            assert_eq!(fleet.five_hour.as_ref().expect("five-hour").used_pct, 62.0);
            // The newer payload carried no seven-day window; the older
            // pane's reading survives instead of being erased.
            assert_eq!(fleet.seven_day.as_ref().expect("seven-day").used_pct, 10.0);
        }
    }

    #[test]
    fn fleetless_or_windowless_readings_aggregate_to_nothing() {
        assert_eq!(fleet_rate_limit(std::iter::empty()), None);
        let empty = RateLimit::default();
        assert_eq!(fleet_rate_limit([(&empty, Instant::now())]), None);
    }

    #[test]
    fn garbage_readings_never_erase_finite_ones() {
        let now = Instant::now();
        let valid = five_only(85.0);
        // A newer non-finite reading is garbage, not a fresher measurement:
        // the finite 85% must survive, not be replaced by an inf gauge.
        let garbage = five_only(f32::INFINITY);
        let fleet = fleet_rate_limit([(&valid, now), (&garbage, now + Duration::from_secs(5))])
            .expect("the finite reading survives");
        assert_eq!(fleet.five_hour.expect("five-hour kept").used_pct, 85.0);
        // Garbage alone reports nothing at all — no window, no footer row.
        assert_eq!(fleet_rate_limit([(&five_only(f32::NAN), now)]), None);
    }

    #[test]
    fn a_stream_sitting_above_a_threshold_notifies_exactly_once() {
        let mut notifier = LimitNotifier::new();
        let mut fired = Vec::new();
        for used in [40.0, 75.0, 75.0, 76.0, 88.0] {
            fired.extend(notifier.observe(Some(&five_only(used))));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].window, LimitWindow::FiveHour);
        assert_eq!(fired[0].alert, ContextAlert::Warn);
        assert_eq!(fired[0].used_pct, 75.0);
    }

    #[test]
    fn crossing_ninety_fires_the_critical_tier_once_more() {
        let mut notifier = LimitNotifier::new();
        assert_eq!(notifier.observe(Some(&five_only(75.0))).len(), 1);
        let critical = notifier.observe(Some(&five_only(91.0)));
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].alert, ContextAlert::Critical);
        assert!(notifier.observe(Some(&five_only(95.0))).is_empty());
    }

    #[test]
    fn jumping_straight_past_both_thresholds_fires_only_the_loud_tier() {
        let mut notifier = LimitNotifier::new();
        let fired = notifier.observe(Some(&five_only(95.0)));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].alert, ContextAlert::Critical);
    }

    #[test]
    fn dropping_below_a_threshold_rearms_the_notice() {
        let mut notifier = LimitNotifier::new();
        assert_eq!(notifier.observe(Some(&five_only(75.0))).len(), 1);
        // Falling back below 70 — a reset zeroes the share, so this is
        // also how a window reset reads — re-arms silently…
        assert!(notifier.observe(Some(&five_only(3.0))).is_empty());
        // …and the next climb notifies again.
        assert_eq!(notifier.observe(Some(&five_only(72.0))).len(), 1);
    }

    #[test]
    fn absent_readings_leave_the_edge_state_alone() {
        let mut notifier = LimitNotifier::new();
        assert_eq!(notifier.observe(Some(&five_only(75.0))).len(), 1);
        // The feed flaps away and back at the same level: no re-notice —
        // absence is "not currently confirmed", not a reset.
        assert!(notifier.observe(None).is_empty());
        assert!(notifier.observe(Some(&five_only(75.0))).is_empty());
        // Garbage is absence too, never a re-arm.
        assert!(notifier.observe(Some(&five_only(f32::NAN))).is_empty());
        assert!(notifier.observe(Some(&five_only(76.0))).is_empty());
    }

    #[test]
    fn windows_cross_thresholds_independently() {
        let mut notifier = LimitNotifier::new();
        let both = RateLimit {
            five_hour: Some(RateLimitWindow {
                used_pct: 75.0,
                resets_in: Some(Duration::from_secs(7200)),
            }),
            seven_day: Some(RateLimitWindow {
                used_pct: 91.0,
                resets_in: None,
            }),
        };
        let fired = notifier.observe(Some(&both));
        assert_eq!(fired.len(), 2);
        assert_eq!(
            (fired[0].window, fired[0].alert),
            (LimitWindow::FiveHour, ContextAlert::Warn)
        );
        assert_eq!(fired[0].resets_in, Some(Duration::from_secs(7200)));
        assert_eq!(
            (fired[1].window, fired[1].alert),
            (LimitWindow::SevenDay, ContextAlert::Critical)
        );
    }
}
