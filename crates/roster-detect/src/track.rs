//! Per-pane reading history and the debouncer that turns raw readings into
//! committed state.
//!
//! Classification alone looks at a single frame; these types carry what a
//! single frame can't: whether the screen changed since last time (a
//! "working" signal), how recently the agent was active (the done-vs-idle
//! call), and enough persistence to refuse to flip the committed state on
//! one noisy frame.
//!
//! [`PaneTracker`] is also the multi-source seam (see
//! `docs/05-claude-native-attention.md`): bridge-fed telemetry supersedes
//! the scrape when present, is never debounced (a statusline payload is a
//! fact, not a noisy frame), and ages out rather than freezing.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use roster_core::{AgentState, Grid, Telemetry};

use crate::detector::StateReading;

/// How many consecutive readings a new state needs before it is committed.
const DEFAULT_COMMIT_AFTER: u32 = 2;
/// Transitions *into* blocked commit faster: a real "needs you" should
/// surface quickly, and a brief false-blocked is less costly than a missed
/// one.
const DEFAULT_BLOCKED_COMMIT_AFTER: u32 = 1;
/// How long a statusline payload keeps riding committed readings. A live
/// feed refreshes far more often than this whenever the agent is doing
/// anything, so a gap this long means the feed is gone (session exited,
/// bridge unhooked) — and stale numbers presented as current are worse than
/// none. A blocked or idle pane can legitimately outlast the window; the
/// next payload restores telemetry instantly, so absence reads as "not
/// currently confirmed", never as an error. Retune against live cadence
/// once the feed is wired (docs/05).
const TELEMETRY_STALE_AFTER: Duration = Duration::from_secs(30);

fn grid_fingerprint(grid: &Grid) -> u64 {
    let mut hasher = DefaultHasher::new();
    grid.lines().hash(&mut hasher);
    hasher.finish()
}

/// What detection remembers about a pane between frames.
#[derive(Debug, Default)]
pub struct History {
    last_fingerprint: Option<u64>,
    last_activity_at: Option<Instant>,
}

impl History {
    /// A history with no recorded frames.
    pub fn new() -> Self {
        History::default()
    }

    /// Record a frame: the raw reading it produced and the grid it was read
    /// from.
    pub fn record(&mut self, state: AgentState, grid: &Grid, at: Instant) {
        self.last_fingerprint = Some(grid_fingerprint(grid));
        if state == AgentState::Working {
            self.last_activity_at = Some(at);
        }
    }

    /// Whether `grid` differs from the previously recorded frame. `None`
    /// until a frame has been recorded — a first frame is not evidence of
    /// activity.
    pub fn content_changed(&self, grid: &Grid) -> Option<bool> {
        self.last_fingerprint
            .map(|prev| prev != grid_fingerprint(grid))
    }

    /// When the agent last produced a `working` reading.
    pub fn last_activity_at(&self) -> Option<Instant> {
        self.last_activity_at
    }
}

/// Turns raw per-frame readings into a committed state that never flips on a
/// single frame.
///
/// A candidate state must persist for a configured number of consecutive
/// readings before it is committed; transitions into
/// [`AgentState::Blocked`] use a lower threshold. While the raw state agrees
/// with the committed state, the committed reason follows the raw reason, so
/// e.g. a working pane's hint stays fresh without any state change.
/// Telemetry never routes through the debouncer: the scrape never carries
/// it, and [`PaneTracker`] attaches the bridge-fed value after debouncing —
/// a statusline payload is a fact, not a noisy frame.
#[derive(Debug)]
pub struct Debouncer {
    committed: StateReading,
    candidate: Option<(AgentState, u32)>,
    commit_after: u32,
    blocked_commit_after: u32,
}

impl Debouncer {
    /// A debouncer with the default thresholds (2 readings, 1 for blocked),
    /// starting from an idle committed state.
    pub fn new() -> Self {
        Debouncer::with_thresholds(DEFAULT_COMMIT_AFTER, DEFAULT_BLOCKED_COMMIT_AFTER)
    }

    /// A debouncer with explicit thresholds. Both must be at least 1.
    pub fn with_thresholds(commit_after: u32, blocked_commit_after: u32) -> Self {
        Debouncer {
            committed: StateReading::default(),
            candidate: None,
            commit_after: commit_after.max(1),
            blocked_commit_after: blocked_commit_after.max(1),
        }
    }

    /// Feed one raw reading; returns the committed reading after applying
    /// it.
    pub fn observe(&mut self, raw: StateReading) -> StateReading {
        if raw.state == self.committed.state {
            self.candidate = None;
            self.committed.reason = raw.reason;
            return self.committed.clone();
        }
        let count = match self.candidate {
            Some((state, count)) if state == raw.state => count + 1,
            _ => 1,
        };
        let threshold = if raw.state == AgentState::Blocked {
            self.blocked_commit_after
        } else {
            self.commit_after
        };
        if count >= threshold {
            self.candidate = None;
            self.committed = raw;
        } else {
            self.candidate = Some((raw.state, count));
        }
        self.committed.clone()
    }

    /// The current committed reading.
    pub fn committed(&self) -> &StateReading {
        &self.committed
    }
}

impl Default for Debouncer {
    fn default() -> Self {
        Debouncer::new()
    }
}

/// Everything detection keeps per pane: history, debouncer, and the pane's
/// freshest bridge telemetry, driven once per refresh via
/// [`PaneTracker::update`].
#[derive(Debug, Default)]
pub struct PaneTracker {
    history: History,
    debouncer: Debouncer,
    /// The freshest statusline payload and when it arrived. Bridge data,
    /// not a scraped signal: it rides committed readings without debouncing
    /// and ages out after [`TELEMETRY_STALE_AFTER`].
    telemetry: Option<(Telemetry, Instant)>,
}

impl PaneTracker {
    /// A tracker for a fresh pane: no history, committed state idle.
    pub fn new() -> Self {
        PaneTracker::default()
    }

    /// Record a statusline payload for this pane; the freshest payload wins
    /// — one stamped older than the held payload is ignored, so out-of-order
    /// delivery cannot regress the data. Telemetry is authoritative bridge
    /// data: it attaches to the reading on the very next
    /// [`PaneTracker::update`] with no debounce delay, and drops back to
    /// `None` once [`TELEMETRY_STALE_AFTER`] passes without a newer payload.
    pub fn set_telemetry(&mut self, telemetry: Telemetry, at: Instant) {
        if self.telemetry.as_ref().is_some_and(|(_, seen)| *seen > at) {
            return;
        }
        self.telemetry = Some((telemetry, at));
    }

    /// Run one detection step: classify the grid, record the frame, debounce,
    /// attach the pane's live telemetry, and return the committed reading.
    pub fn update(
        &mut self,
        detector: &crate::detector::Detector,
        kind: crate::detector::AgentKind,
        grid: &Grid,
        at: Instant,
    ) -> StateReading {
        let raw = detector.classify(kind, grid, &self.history, at);
        self.history.record(raw.state, grid, at);
        let mut reading = self.debouncer.observe(raw);
        // A payload past the staleness window stops being asserted instead
        // of freezing its last numbers; a held one supersedes the scrape's
        // `None`. The reading's telemetry always equals the post-purge slot.
        self.telemetry = self
            .telemetry
            .take()
            .filter(|(_, seen)| at.saturating_duration_since(*seen) <= TELEMETRY_STALE_AFTER);
        reading.telemetry = self
            .telemetry
            .as_ref()
            .map(|(telemetry, _)| telemetry.clone());
        reading
    }

    /// The scrape-committed reading with the pane's held telemetry attached.
    /// Aging is evaluated by [`PaneTracker::update`], so a quiet feed's last
    /// payload lingers here until the next update purges it.
    pub fn committed(&self) -> StateReading {
        let mut reading = self.debouncer.committed().clone();
        reading.telemetry = self
            .telemetry
            .as_ref()
            .map(|(telemetry, _)| telemetry.clone());
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn reading(state: AgentState, reason: &str) -> StateReading {
        StateReading {
            state,
            reason: Some(reason.to_string()),
            telemetry: None,
        }
    }

    /// A detector with one working pattern, plus its kind — enough to drive
    /// a [`PaneTracker`] without the builtin config.
    fn tracker_detector() -> (crate::detector::Detector, crate::detector::AgentKind) {
        let detector = crate::detector::Detector::from_toml(
            r#"
            [test-agent]
            match_command = ["ta"]
            working = ['SPINNING']
            "#,
        )
        .expect("test agents.toml parses");
        let kind = detector.identify("ta").expect("ta identifies");
        (detector, kind)
    }

    fn sample_telemetry(context_pct: f32) -> Telemetry {
        Telemetry {
            model: Some("Opus".to_string()),
            context_pct: Some(context_pct),
            ..Telemetry::default()
        }
    }

    #[test]
    fn history_reports_content_changes() {
        let mut history = History::new();
        let a = Grid::from_text("one");
        let b = Grid::from_text("two");
        assert_eq!(history.content_changed(&a), None);
        history.record(AgentState::Idle, &a, Instant::now());
        assert_eq!(history.content_changed(&a), Some(false));
        assert_eq!(history.content_changed(&b), Some(true));
    }

    #[test]
    fn history_records_activity_only_for_working() {
        let mut history = History::new();
        let grid = Grid::from_text("x");
        let t0 = Instant::now();
        history.record(AgentState::Blocked, &grid, t0);
        assert_eq!(history.last_activity_at(), None);
        history.record(AgentState::Working, &grid, t0 + Duration::from_secs(1));
        assert_eq!(
            history.last_activity_at(),
            Some(t0 + Duration::from_secs(1))
        );
        history.record(AgentState::Idle, &grid, t0 + Duration::from_secs(2));
        assert_eq!(
            history.last_activity_at(),
            Some(t0 + Duration::from_secs(1))
        );
    }

    #[test]
    fn single_frame_flicker_does_not_flip_state() {
        let mut d = Debouncer::new();
        d.observe(reading(AgentState::Working, "compiling"));
        d.observe(reading(AgentState::Working, "compiling"));
        assert_eq!(d.committed().state, AgentState::Working);

        let seen = d.observe(reading(AgentState::Idle, ""));
        assert_eq!(seen.state, AgentState::Working);
        let seen = d.observe(reading(AgentState::Working, "compiling"));
        assert_eq!(seen.state, AgentState::Working);
    }

    #[test]
    fn new_state_commits_after_threshold() {
        let mut d = Debouncer::new();
        assert_eq!(d.committed().state, AgentState::Idle);
        let seen = d.observe(reading(AgentState::Working, "a"));
        assert_eq!(seen.state, AgentState::Idle);
        let seen = d.observe(reading(AgentState::Working, "b"));
        assert_eq!(seen.state, AgentState::Working);
        assert_eq!(seen.reason.as_deref(), Some("b"));
    }

    #[test]
    fn blocked_commits_on_first_reading() {
        let mut d = Debouncer::new();
        d.observe(reading(AgentState::Working, "x"));
        d.observe(reading(AgentState::Working, "x"));
        let seen = d.observe(reading(AgentState::Blocked, "Allow edit?"));
        assert_eq!(seen.state, AgentState::Blocked);
        assert_eq!(seen.reason.as_deref(), Some("Allow edit?"));
    }

    #[test]
    fn alternating_states_never_commit() {
        let mut d = Debouncer::new();
        for _ in 0..5 {
            assert_eq!(
                d.observe(reading(AgentState::Working, "w")).state,
                AgentState::Idle
            );
            assert_eq!(
                d.observe(reading(AgentState::Done, "d")).state,
                AgentState::Idle
            );
        }
    }

    #[test]
    fn reason_updates_without_state_change() {
        let mut d = Debouncer::new();
        d.observe(reading(AgentState::Working, "compiling"));
        d.observe(reading(AgentState::Working, "compiling"));
        let seen = d.observe(reading(AgentState::Working, "running tests"));
        assert_eq!(seen.state, AgentState::Working);
        assert_eq!(seen.reason.as_deref(), Some("running tests"));
    }

    #[test]
    fn candidate_resets_when_interrupted_by_committed_state() {
        let mut d = Debouncer::new();
        d.observe(reading(AgentState::Working, "w"));
        d.observe(reading(AgentState::Working, "w"));
        d.observe(reading(AgentState::Done, "d"));
        d.observe(reading(AgentState::Working, "w"));
        let seen = d.observe(reading(AgentState::Done, "d"));
        assert_eq!(seen.state, AgentState::Working);
    }

    #[test]
    fn telemetry_supersedes_when_present() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");

        // The payload rides the very next reading — even one whose scraped
        // state is still mid-debounce (working is only a candidate here).
        // Bridge data is a fact, not a noisy frame; it never waits.
        tracker.set_telemetry(sample_telemetry(62.0), t0);
        let seen = tracker.update(&detector, kind, &grid, t0);
        assert_eq!(seen.state, AgentState::Idle);
        assert_eq!(seen.telemetry, Some(sample_telemetry(62.0)));
        assert_eq!(tracker.committed().telemetry, Some(sample_telemetry(62.0)));

        // The freshest payload wins over the one it replaces.
        tracker.set_telemetry(sample_telemetry(58.5), t0 + Duration::from_secs(1));
        let seen = tracker.update(&detector, kind, &grid, t0 + Duration::from_secs(1));
        assert_eq!(seen.state, AgentState::Working);
        assert_eq!(seen.telemetry, Some(sample_telemetry(58.5)));

        // An out-of-order payload stamped older than the held one is
        // ignored — arrival order cannot regress the data.
        tracker.set_telemetry(sample_telemetry(99.0), t0);
        let seen = tracker.update(&detector, kind, &grid, t0 + Duration::from_secs(2));
        assert_eq!(seen.telemetry, Some(sample_telemetry(58.5)));
    }

    #[test]
    fn scrape_only_unchanged() {
        // A pane with no bridge feed reads exactly as before the field
        // existed: scraped state and reason, telemetry never `Some`.
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");

        let seen = tracker.update(&detector, kind, &grid, t0);
        assert_eq!(seen.telemetry, None);
        let seen = tracker.update(&detector, kind, &grid, t0 + Duration::from_secs(1));
        assert_eq!(seen.state, AgentState::Working);
        assert_eq!(seen.telemetry, None);
        assert_eq!(tracker.committed().telemetry, None);
        assert_eq!(StateReading::default().telemetry, None);
    }

    #[test]
    fn stale_telemetry_ages_out() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");
        tracker.set_telemetry(sample_telemetry(41.0), t0);

        // At the window boundary the payload still rides (inclusive)...
        let seen = tracker.update(&detector, kind, &grid, t0 + TELEMETRY_STALE_AFTER);
        assert_eq!(seen.telemetry, Some(sample_telemetry(41.0)));

        // ...one second past it, the reading drops back to None.
        let t_stale = t0 + TELEMETRY_STALE_AFTER + Duration::from_secs(1);
        let seen = tracker.update(&detector, kind, &grid, t_stale);
        assert_eq!(seen.telemetry, None);
        assert_eq!(tracker.committed().telemetry, None);

        // A fresh payload restores it instantly.
        let t_fresh = t_stale + Duration::from_secs(1);
        tracker.set_telemetry(sample_telemetry(12.0), t_fresh);
        let seen = tracker.update(&detector, kind, &grid, t_fresh);
        assert_eq!(seen.telemetry, Some(sample_telemetry(12.0)));
    }
}
