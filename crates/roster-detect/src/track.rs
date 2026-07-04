//! Per-pane reading history and the debouncer that turns raw readings into
//! committed state.
//!
//! Classification alone looks at a single frame; these types carry what a
//! single frame can't: whether the screen changed since last time (a
//! "working" signal), how recently the agent was active (the done-vs-idle
//! call), and enough persistence to refuse to flip the committed state on
//! one noisy frame.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use roster_core::{AgentState, Grid};

use crate::detector::StateReading;

/// How many consecutive readings a new state needs before it is committed.
const DEFAULT_COMMIT_AFTER: u32 = 2;
/// Transitions *into* blocked commit faster: a real "needs you" should
/// surface quickly, and a brief false-blocked is less costly than a missed
/// one.
const DEFAULT_BLOCKED_COMMIT_AFTER: u32 = 1;

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

/// Everything detection keeps per pane: history plus debouncer, driven once
/// per refresh via [`PaneTracker::update`].
#[derive(Debug, Default)]
pub struct PaneTracker {
    history: History,
    debouncer: Debouncer,
}

impl PaneTracker {
    /// A tracker for a fresh pane: no history, committed state idle.
    pub fn new() -> Self {
        PaneTracker::default()
    }

    /// Run one detection step: classify the grid, record the frame, debounce,
    /// and return the committed reading.
    pub fn update(
        &mut self,
        detector: &crate::detector::Detector,
        kind: crate::detector::AgentKind,
        grid: &Grid,
        at: Instant,
    ) -> StateReading {
        let raw = detector.classify(kind, grid, &self.history, at);
        self.history.record(raw.state, grid, at);
        self.debouncer.observe(raw)
    }

    /// The committed reading as of the last update.
    pub fn committed(&self) -> &StateReading {
        self.debouncer.committed()
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
}
