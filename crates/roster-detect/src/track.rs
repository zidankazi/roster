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

use roster_core::{AgentState, Grid, RateLimit, Telemetry};

use crate::config::AgentConfig;
use crate::detector::StateReading;

/// How many consecutive readings a new state needs before it is committed.
const DEFAULT_COMMIT_AFTER: u32 = 2;
/// Transitions *into* blocked commit faster: a real "needs you" should
/// surface quickly, and a brief false-blocked is less costly than a missed
/// one.
const DEFAULT_BLOCKED_COMMIT_AFTER: u32 = 1;
/// How long the fast-moving badge readings (model, context %, cost) keep
/// riding committed readings. A live feed refreshes far more often than
/// this whenever the agent is doing anything, so a gap this long means the
/// numbers stopped moving with reality — and stale numbers presented as
/// current are worse than none. Rate limits deliberately outlive this
/// window (see [`PaneTracker::asserted_telemetry`]): a blocked or idle pane
/// legitimately goes quiet for far longer, and account usage cannot move
/// while every feed is quiet, so limits vanishing whenever the fleet idles
/// for half a minute was a bug, not honesty. The next payload restores the
/// full reading instantly, so absence reads as "not currently confirmed",
/// never as an error.
const TELEMETRY_STALE_AFTER: Duration = Duration::from_secs(30);

/// Hash of the grid rows that count as agent output. Rows matching any
/// `activity.ignore` pattern are excluded: the composer echoing keystrokes
/// and status chrome toggling change the screen without the agent doing
/// anything, and counting those rows as activity reads a human typing an
/// unsent prompt as working. Blank rows are excluded too — chrome that
/// vanishes leaves a blank behind, and a blank is not readable content.
/// When `activity.ignore_region` is set, the rows from the bottom-most
/// start match through the first following end match (the composer box:
/// prompt row, wrapped continuation rows, closing border) are excluded
/// wholesale — but rows *below* the region (a background-task tray) still
/// count. The remaining rows hash with their row index, so real content
/// merely *moving* (output scrolling, a blank line pushed into the middle)
/// still registers as activity.
fn activity_fingerprint(grid: &Grid, config: &AgentConfig) -> u64 {
    let lines = grid.lines();
    let region = config
        .activity_ignore_region
        .as_ref()
        .and_then(|(start, end)| {
            let first = lines.iter().rposition(|line| start.is_match(line))?;
            let last = lines[first + 1..]
                .iter()
                .position(|line| end.is_match(line))
                .map(|offset| first + 1 + offset)
                .unwrap_or(lines.len() - 1);
            Some(first..=last)
        });
    let mut hasher = DefaultHasher::new();
    for (row, line) in lines
        .iter()
        .enumerate()
        .filter(|(row, _)| !region.as_ref().is_some_and(|region| region.contains(row)))
        .filter(|(_, line)| !line.is_empty())
        .filter(|(_, line)| {
            !config
                .activity_ignore
                .iter()
                .any(|pattern| pattern.is_match(line))
        })
    {
        (row, line).hash(&mut hasher);
    }
    hasher.finish()
}

/// The fingerprint of a screen with no countable rows. A blank grid — or
/// one that is all ignored chrome — hashes to this constant, and two such
/// frames matching means "nothing painted yet", never "the screen held
/// still", so the settle latch must not open on it.
fn empty_fingerprint() -> u64 {
    DefaultHasher::new().finish()
}

/// What detection remembers about a pane between frames.
///
/// A `History` must not outlive its pane's child process: the settle latch
/// is per-child, and the binary keeps this true by building a fresh
/// [`PaneTracker`] on attach and restart.
#[derive(Debug, Default)]
pub struct History {
    last_fingerprint: Option<u64>,
    last_activity_at: Option<Instant>,
    settled: bool,
}

impl History {
    /// A history with no recorded frames.
    pub fn new() -> Self {
        History::default()
    }

    /// Record a frame: the reading to bookkeep and the grid it was read
    /// from. `config` supplies the agent's activity filters; pass the same
    /// agent's config to [`History::content_changed`], or the fingerprints
    /// are not comparable.
    ///
    /// `state` feeds the activity stamp behind the done-vs-idle call.
    /// [`PaneTracker::update`] passes the *committed* (post-debounce) state,
    /// so one noisy frame can never arm the done window — the same
    /// don't-trust-one-frame contract the debouncer gives committed state.
    pub fn record(&mut self, state: AgentState, grid: &Grid, config: &AgentConfig, at: Instant) {
        let fingerprint = activity_fingerprint(grid, config);
        if self.last_fingerprint == Some(fingerprint) && fingerprint != empty_fingerprint() {
            self.settled = true;
        }
        self.last_fingerprint = Some(fingerprint);
        if state == AgentState::Working {
            self.last_activity_at = Some(at);
        }
    }

    /// Whether `grid`'s activity rows differ from the previously recorded
    /// frame's. `None` until a frame has been recorded — a first frame is
    /// not evidence of activity. `config` must be the same agent's config
    /// that [`History::record`] was given.
    pub fn content_changed(&self, grid: &Grid, config: &AgentConfig) -> Option<bool> {
        self.last_fingerprint
            .map(|prev| prev != activity_fingerprint(grid, config))
    }

    /// Whether two consecutive recorded frames have ever matched with
    /// content on screen — the pane has painted something and held it still
    /// for at least one poll. Until then, every frame differs from the last
    /// because the program is painting its initial UI (or nothing has
    /// painted yet — blank frames match each other but prove nothing), and
    /// those changes are not evidence of work: without this gate a freshly
    /// spawned agent's own banner paint reads as working, and the prompt
    /// that follows reads as done instead of idle.
    pub fn has_settled(&self) -> bool {
        self.settled
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
    /// The freshest statusline payload as it arrived, and when. Bridge
    /// data, not a scraped signal: it rides committed readings without
    /// debouncing, filtered through [`PaneTracker::asserted_telemetry`] —
    /// the held value stays the original so aging always derives from the
    /// arrival stamp, never compounds.
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
    /// [`PaneTracker::update`] with no debounce delay, and thins out as it
    /// ages ([`PaneTracker::asserted_telemetry`]) instead of freezing.
    pub fn set_telemetry(&mut self, telemetry: Telemetry, at: Instant) {
        if self.telemetry.as_ref().is_some_and(|(_, seen)| *seen > at) {
            return;
        }
        self.telemetry = Some((telemetry, at));
    }

    /// Run one detection step: classify the grid, debounce, record the frame
    /// under the committed state, attach the pane's live telemetry, and
    /// return the committed reading.
    pub fn update(
        &mut self,
        detector: &crate::detector::Detector,
        kind: crate::detector::AgentKind,
        grid: &Grid,
        at: Instant,
    ) -> StateReading {
        let raw = detector.classify(kind, grid, &self.history, at);
        let mut reading = self.debouncer.observe(raw);
        // The committed state — not the raw frame — feeds the activity
        // stamp: startup chrome landing seconds after the prompt (the MCP
        // notice), or a wrapped composer shifting the transcript a row,
        // is one changed frame, and one frame must never arm the done
        // window. Real work commits working within two polls and stamps
        // from then on.
        self.history
            .record(reading.state, grid, detector.agent(kind), at);
        // A payload with nothing left to assert stops being held instead of
        // freezing its last numbers; a held one supersedes the scrape's
        // `None`. The reading's telemetry always equals the post-purge
        // assertion.
        reading.telemetry = self.asserted_telemetry(at);
        if reading.telemetry.is_none() {
            self.telemetry = None;
        }
        reading
    }

    /// What the held payload still asserts at `now`, or `None` when nothing
    /// does. Within [`TELEMETRY_STALE_AFTER`] the full payload rides, its
    /// rate-limit countdowns aged to `now`; past it only the rate-limit
    /// windows with a reported reset survive — account usage cannot move
    /// while every feed is quiet, and the countdown re-derives from the
    /// arrival stamp — each dying the moment its reset passes
    /// ([`RateLimit::aged`]), since the reset zeroes the used share the
    /// reading holds. A window without a reported reset has no horizon to
    /// age against and keeps the fast-field rule. The clock is monotonic:
    /// a countdown pauses across a system sleep and can overstate the
    /// remaining wait until the next payload corrects it — accepted over a
    /// wall clock's misfires on clock adjustments.
    fn asserted_telemetry(&self, now: Instant) -> Option<Telemetry> {
        let (held, seen) = self.telemetry.as_ref()?;
        let elapsed = now.saturating_duration_since(*seen);
        let rate_limit = held
            .rate_limit
            .as_ref()
            .and_then(|limit| limit.aged(elapsed));
        if elapsed <= TELEMETRY_STALE_AFTER {
            // A payload whose only reading just died (a window that arrived
            // already reset) asserts nothing — never an all-empty reading.
            return Some(Telemetry {
                rate_limit,
                ..held.clone()
            })
            .filter(|telemetry| *telemetry != Telemetry::default());
        }
        let rate_limit = rate_limit.and_then(|limit| {
            let five_hour = limit.five_hour.filter(|w| w.resets_in.is_some());
            let seven_day = limit.seven_day.filter(|w| w.resets_in.is_some());
            (five_hour.is_some() || seven_day.is_some()).then_some(RateLimit {
                five_hour,
                seven_day,
            })
        });
        rate_limit.map(|rate_limit| Telemetry {
            rate_limit: Some(rate_limit),
            ..Telemetry::default()
        })
    }

    /// The telemetry the tracker asserts at `now`
    /// ([`PaneTracker::asserted_telemetry`]) with the payload's arrival
    /// stamp — the freshness key fleet-level aggregation orders by (rate
    /// limits are account-scoped, so `roster_core::fleet_rate_limit` merges
    /// panes by recency). The values are current — countdowns aged to
    /// `now`, dead readings dropped — while the stamp stays the arrival, so
    /// an aged copy can never outrank a genuinely newer payload.
    pub fn telemetry_stamped(&self, now: Instant) -> Option<(Telemetry, Instant)> {
        let (_, seen) = self.telemetry.as_ref()?;
        let seen = *seen;
        self.asserted_telemetry(now)
            .map(|telemetry| (telemetry, seen))
    }

    /// The scrape-committed reading with what the pane's held payload still
    /// asserts at `now` attached ([`PaneTracker::asserted_telemetry`]).
    pub fn committed(&self, now: Instant) -> StateReading {
        let mut reading = self.debouncer.committed().clone();
        reading.telemetry = self.asserted_telemetry(now);
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

    /// A one-agent config compiled from the given `agents.toml` body, for
    /// driving [`History`] with specific activity filters.
    fn agent_config(toml: &str) -> AgentConfig {
        crate::config::parse_agents(toml)
            .expect("test agents.toml parses")
            .remove(0)
    }

    /// A config with no activity filters at all — every row counts.
    fn bare_config() -> AgentConfig {
        agent_config("[bare]\nmatch_command = [\"bare\"]")
    }

    #[test]
    fn history_reports_content_changes() {
        let bare = bare_config();
        let mut history = History::new();
        let a = Grid::from_text("one");
        let b = Grid::from_text("two");
        assert_eq!(history.content_changed(&a, &bare), None);
        history.record(AgentState::Idle, &a, &bare, Instant::now());
        assert_eq!(history.content_changed(&a, &bare), Some(false));
        assert_eq!(history.content_changed(&b, &bare), Some(true));
    }

    #[test]
    fn ignored_rows_do_not_count_as_change() {
        // The composer row echoes keystrokes; excluding it from the
        // fingerprint keeps a human typing from reading as activity, while
        // a change on any other row still registers.
        let config = agent_config(
            r#"
            [ta]
            match_command = ["ta"]
            activity.ignore = ['^❯']
            "#,
        );
        let mut history = History::new();
        let resting = Grid::from_text("output\n❯");
        let typing = Grid::from_text("output\n❯ how do I");
        let more_output = Grid::from_text("more output\n❯ how do I");
        history.record(AgentState::Idle, &resting, &config, Instant::now());
        assert_eq!(history.content_changed(&typing, &config), Some(false));
        assert_eq!(history.content_changed(&more_output, &config), Some(true));
    }

    #[test]
    fn composer_region_rows_do_not_count_as_change() {
        // ignore_region excludes the bottom-most prompt row through the
        // next border row: wrapped continuation rows of an unsent prompt
        // change with every keystroke but carry no prompt glyph of their
        // own. Content above the region still counts, a prompt row higher
        // up (a transcript echo) does not anchor the region, and rows
        // *below* the border (a background-task tray) still count.
        let config = agent_config(
            r#"
            [ta]
            match_command = ["ta"]
            activity.ignore_region = ['^❯', '^─+$']
            "#,
        );
        let grid = |above: &str, wrapped: &str, tray: &str| {
            Grid::from_text(&format!(
                "❯ old echo\n{above}\n❯ typing a very long\n{wrapped}\n─────\n{tray}"
            ))
        };
        let mut history = History::new();
        history.record(
            AgentState::Idle,
            &grid("output", "  wrapped", "tray idle"),
            &config,
            Instant::now(),
        );
        let typing_more = grid("output", "  wrapped more", "tray idle");
        assert_eq!(history.content_changed(&typing_more, &config), Some(false));
        let new_output = grid("fresh", "  wrapped", "tray idle");
        assert_eq!(history.content_changed(&new_output, &config), Some(true));
        let tray_tick = grid("output", "  wrapped", "tray BUSY");
        assert_eq!(history.content_changed(&tray_tick, &config), Some(true));
    }

    #[test]
    fn content_moving_rows_still_counts_as_change() {
        // Rows hash with their position: the same non-blank lines shifted
        // down a row (output scrolling, a blank pushed into the middle) is
        // real screen movement, not a no-op.
        let bare = bare_config();
        let mut history = History::new();
        let before = Grid::from_text("phase one\n\nphase two");
        let shifted = Grid::from_text("\nphase one\n\nphase two");
        history.record(AgentState::Idle, &before, &bare, Instant::now());
        assert_eq!(history.content_changed(&shifted, &bare), Some(true));
    }

    #[test]
    fn history_settles_only_after_a_repeated_content_frame() {
        let bare = bare_config();
        let mut history = History::new();
        let t0 = Instant::now();
        assert!(!history.has_settled());
        // Blank frames match each other, but a screen nothing has painted
        // on is not a settle — a slow-starting child must not open the
        // gate before its first output.
        history.record(AgentState::Idle, &Grid::new(80, 24), &bare, t0);
        history.record(AgentState::Idle, &Grid::new(80, 24), &bare, t0);
        assert!(!history.has_settled(), "blank frames are not a settle");
        history.record(AgentState::Idle, &Grid::from_text("one"), &bare, t0);
        assert!(!history.has_settled());
        history.record(AgentState::Idle, &Grid::from_text("two"), &bare, t0);
        assert!(
            !history.has_settled(),
            "frames that keep differing are a paint, not a settle"
        );
        history.record(AgentState::Idle, &Grid::from_text("two"), &bare, t0);
        assert!(history.has_settled());
        // Settling is one-way: later movement does not close the gate.
        history.record(AgentState::Working, &Grid::from_text("three"), &bare, t0);
        assert!(history.has_settled());
    }

    /// A detector with an idle prompt pattern and no working patterns, for
    /// driving the change-signal path end to end through [`PaneTracker`].
    fn prompt_only_detector() -> (crate::detector::Detector, crate::detector::AgentKind) {
        let detector = crate::detector::Detector::from_toml(
            r#"
            [ta]
            match_command = ["ta"]
            idle = ['^>']
            "#,
        )
        .expect("test agents.toml parses");
        let kind = detector.identify("ta").expect("ta identifies");
        (detector, kind)
    }

    #[test]
    fn a_single_changed_frame_does_not_stamp_activity() {
        // One changed frame — startup chrome landing after the prompt, a
        // wrap-boundary transcript shift — reads as raw working but never
        // commits, and activity stamps from the committed state: the
        // settled prompt that follows must read idle, not done.
        let (detector, kind) = prompt_only_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let resting = Grid::from_text("banner\n>");
        tracker.update(&detector, kind, &resting, at(0));
        tracker.update(&detector, kind, &resting, at(1));
        let notice = Grid::from_text("banner\nnotice\n>");
        let seen = tracker.update(&detector, kind, &notice, at(2));
        assert_eq!(seen.state, AgentState::Idle);
        let seen = tracker.update(&detector, kind, &notice, at(3));
        assert_eq!(seen.state, AgentState::Idle, "no activity was stamped");
        let seen = tracker.update(&detector, kind, &notice, at(4));
        assert_eq!(seen.state, AgentState::Idle);
    }

    #[test]
    fn sustained_change_still_stamps_and_reads_done() {
        // The counterpart: output moving across consecutive polls commits
        // working, stamps activity, and the prompt that follows reads done.
        let (detector, kind) = prompt_only_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let resting = Grid::from_text("banner\n>");
        tracker.update(&detector, kind, &resting, at(0));
        tracker.update(&detector, kind, &resting, at(1));
        let out = |line: &str| Grid::from_text(&format!("banner\n{line}\n>"));
        tracker.update(&detector, kind, &out("step one"), at(2));
        let seen = tracker.update(&detector, kind, &out("step two"), at(3));
        assert_eq!(seen.state, AgentState::Working);
        let seen = tracker.update(&detector, kind, &out("finished"), at(4));
        assert_eq!(seen.state, AgentState::Working);
        tracker.update(&detector, kind, &out("finished"), at(5));
        let seen = tracker.update(&detector, kind, &out("finished"), at(6));
        assert_eq!(seen.state, AgentState::Done);
    }

    #[test]
    fn history_records_activity_only_for_working() {
        let bare = bare_config();
        let mut history = History::new();
        let grid = Grid::from_text("x");
        let t0 = Instant::now();
        history.record(AgentState::Blocked, &grid, &bare, t0);
        assert_eq!(history.last_activity_at(), None);
        history.record(
            AgentState::Working,
            &grid,
            &bare,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            history.last_activity_at(),
            Some(t0 + Duration::from_secs(1))
        );
        history.record(AgentState::Idle, &grid, &bare, t0 + Duration::from_secs(2));
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
        assert_eq!(
            tracker.committed(t0).telemetry,
            Some(sample_telemetry(62.0))
        );

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
    fn pane_without_telemetry_reads_unchanged() {
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
        assert_eq!(
            tracker.committed(t0 + Duration::from_secs(1)).telemetry,
            None
        );
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
        assert_eq!(tracker.committed(t_stale).telemetry, None);

        // A fresh payload restores it instantly.
        let t_fresh = t_stale + Duration::from_secs(1);
        tracker.set_telemetry(sample_telemetry(12.0), t_fresh);
        let seen = tracker.update(&detector, kind, &grid, t_fresh);
        assert_eq!(seen.telemetry, Some(sample_telemetry(12.0)));
    }

    #[test]
    fn stamped_telemetry_carries_its_arrival_and_follows_the_purge() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        assert_eq!(tracker.telemetry_stamped(t0), None);

        tracker.set_telemetry(sample_telemetry(41.0), t0);
        let (telemetry, at) = tracker.telemetry_stamped(t0).expect("payload held");
        assert_eq!(telemetry, sample_telemetry(41.0));
        assert_eq!(at, t0);

        // The stamp follows update's purge, so a fleet aggregation reading
        // this after the detect tick can never see what the cards don't.
        let grid = Grid::from_text("SPINNING away");
        let t_stale = t0 + TELEMETRY_STALE_AFTER + Duration::from_secs(1);
        tracker.update(&detector, kind, &grid, t_stale);
        assert_eq!(tracker.telemetry_stamped(t_stale), None);
    }

    /// A telemetry payload carrying one five-hour rate-limit window.
    fn limited_telemetry(used_pct: f32, resets_in: Option<Duration>) -> Telemetry {
        Telemetry {
            model: Some("Opus".to_string()),
            context_pct: Some(62.0),
            rate_limit: Some(RateLimit {
                five_hour: Some(roster_core::RateLimitWindow {
                    used_pct,
                    resets_in,
                }),
                seven_day: None,
            }),
            ..Telemetry::default()
        }
    }

    #[test]
    fn rate_limits_outlive_the_stale_window_with_a_live_countdown() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");
        tracker.set_telemetry(limited_telemetry(75.0, Some(Duration::from_secs(7200))), t0);

        // Past the stale window the fast fields drop, but the rate limit —
        // usage can't move while the feed is quiet — keeps asserting, its
        // countdown re-derived from the arrival stamp.
        let t_quiet = t0 + Duration::from_secs(600);
        let seen = tracker.update(&detector, kind, &grid, t_quiet);
        let telemetry = seen.telemetry.expect("rate limit still asserted");
        assert_eq!(telemetry.model, None);
        assert_eq!(telemetry.context_pct, None);
        let window = telemetry
            .rate_limit
            .clone()
            .expect("rate limit held")
            .five_hour
            .expect("five-hour window held");
        assert_eq!(window.used_pct, 75.0);
        assert_eq!(window.resets_in, Some(Duration::from_secs(6600)));

        // The fleet view sees the same aged reading under the arrival stamp.
        let (stamped, at) = tracker.telemetry_stamped(t_quiet).expect("still stamped");
        assert_eq!(at, t0);
        assert_eq!(stamped.rate_limit, telemetry.rate_limit);
    }

    #[test]
    fn countdowns_age_within_the_fresh_window_too() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");
        tracker.set_telemetry(limited_telemetry(40.0, Some(Duration::from_secs(7200))), t0);

        let seen = tracker.update(&detector, kind, &grid, t0 + Duration::from_secs(10));
        let telemetry = seen.telemetry.expect("fresh payload rides");
        // Fresh: the fast fields still ride...
        assert_eq!(telemetry.model.as_deref(), Some("Opus"));
        // ...and the countdown is already current, not the arrival value.
        let window = telemetry
            .rate_limit
            .expect("rate limit held")
            .five_hour
            .expect("five-hour window held");
        assert_eq!(window.resets_in, Some(Duration::from_secs(7190)));
    }

    #[test]
    fn a_rate_limit_dies_when_its_reset_passes() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");
        tracker.set_telemetry(limited_telemetry(90.0, Some(Duration::from_secs(60))), t0);

        // The window reset while the feed was quiet: its used share is
        // obsolete, so nothing is asserted and the slot purges.
        let t_reset = t0 + Duration::from_secs(61);
        let seen = tracker.update(&detector, kind, &grid, t_reset);
        assert_eq!(seen.telemetry, None);
        assert_eq!(tracker.telemetry_stamped(t_reset), None);
    }

    #[test]
    fn a_rate_limit_without_a_reset_keeps_the_stale_rule() {
        let (detector, kind) = tracker_detector();
        let mut tracker = PaneTracker::new();
        let t0 = Instant::now();
        let grid = Grid::from_text("SPINNING away");
        tracker.set_telemetry(limited_telemetry(75.0, None), t0);

        // No reported reset means no horizon to age against: the window
        // rides while fresh and drops with the fast fields.
        let seen = tracker.update(&detector, kind, &grid, t0 + Duration::from_secs(5));
        assert!(seen.telemetry.expect("fresh rides").rate_limit.is_some());
        let t_stale = t0 + TELEMETRY_STALE_AFTER + Duration::from_secs(1);
        let seen = tracker.update(&detector, kind, &grid, t_stale);
        assert_eq!(seen.telemetry, None);
    }
}
