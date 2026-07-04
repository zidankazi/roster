//! The contract suite: captured agent screens in `tests/fixtures/`, each
//! classified against the shipped `agents.toml` and asserted against its
//! expected reading. No PTY, no subprocess — grids come straight from text.

use std::path::Path;
use std::time::{Duration, Instant};

use roster_core::{AgentState, Grid};
use roster_detect::{Detector, History, PaneTracker, StateReading};

fn fixture(agent: &str, name: &str) -> Grid {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(agent)
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    Grid::from_text(&text)
}

/// Classify a fixture with no prior history — the steady-state reading.
fn classify_fresh(agent: &str, command: &str, name: &str) -> StateReading {
    let detector = Detector::builtin();
    let kind = detector.identify(command).expect("command identifies");
    detector.classify(kind, &fixture(agent, name), &History::new(), Instant::now())
}

/// Classify a fixture as if the agent produced a `working` reading
/// `secs_ago` seconds ago and the screen has settled since (content
/// unchanged between that frame and this one).
fn classify_after_activity(
    agent: &str,
    command: &str,
    name: &str,
    secs_ago: u64,
) -> StateReading {
    let detector = Detector::builtin();
    let kind = detector.identify(command).expect("command identifies");
    let grid = fixture(agent, name);
    let t0 = Instant::now();
    let mut history = History::new();
    history.record(AgentState::Working, &grid, t0);
    detector.classify(kind, &grid, &history, t0 + Duration::from_secs(secs_ago))
}

fn assert_reading(reading: StateReading, state: AgentState, reason: Option<&str>) {
    assert_eq!(reading.state, state);
    assert_eq!(reading.reason.as_deref(), reason);
}

#[test]
fn claude_blocked_on_proceed_prompt() {
    assert_reading(
        classify_fresh("claude-code", "claude", "blocked_proceed.txt"),
        AgentState::Blocked,
        Some("Do you want to proceed?"),
    );
}

#[test]
fn claude_blocked_on_allow_edit() {
    assert_reading(
        classify_fresh("claude-code", "claude", "blocked_allow_edit.txt"),
        AgentState::Blocked,
        Some("Allow edit to src/config.ts?"),
    );
}

#[test]
fn claude_blocked_outranks_visible_spinner() {
    assert_reading(
        classify_fresh("claude-code", "claude", "blocked_wins_over_working.txt"),
        AgentState::Blocked,
        Some("Do you want to proceed?"),
    );
}

#[test]
fn claude_working_from_esc_hint() {
    assert_reading(
        classify_fresh("claude-code", "claude", "working_esc_hint.txt"),
        AgentState::Working,
        Some("✳ Compiling roster-core… (esc to interrupt)"),
    );
}

#[test]
fn claude_working_from_spinner_glyph() {
    assert_reading(
        classify_fresh("claude-code", "claude", "working_spinner.txt"),
        AgentState::Working,
        Some("⠹ Reticulating… (ctrl+c to interrupt)"),
    );
}

#[test]
fn claude_idle_at_rest() {
    assert_reading(
        classify_fresh("claude-code", "claude", "idle_prompt.txt"),
        AgentState::Idle,
        None,
    );
}

#[test]
fn claude_done_shortly_after_activity() {
    assert_reading(
        classify_after_activity("claude-code", "claude", "done_after_task.txt", 3),
        AgentState::Done,
        Some("✓ Done. 3 files changed, 120 insertions."),
    );
}

#[test]
fn claude_done_window_boundary_is_inclusive() {
    // claude-code sets done.after_activity_secs = 8
    assert_eq!(
        classify_after_activity("claude-code", "claude", "done_after_task.txt", 8).state,
        AgentState::Done,
    );
    assert_eq!(
        classify_after_activity("claude-code", "claude", "done_after_task.txt", 9).state,
        AgentState::Idle,
    );
}

#[test]
fn claude_stale_prompt_is_idle_not_done() {
    assert_reading(
        classify_after_activity("claude-code", "claude", "done_after_task.txt", 30),
        AgentState::Idle,
        None,
    );
}

#[test]
fn codex_blocked_on_command_approval() {
    assert_reading(
        classify_fresh("codex", "codex", "blocked_approve.txt"),
        AgentState::Blocked,
        Some("Approve this command?"),
    );
}

#[test]
fn codex_working_while_running() {
    assert_reading(
        classify_fresh("codex", "codex", "working_running.txt"),
        AgentState::Working,
        Some("Running cargo check…"),
    );
}

#[test]
fn codex_working_while_thinking() {
    assert_reading(
        classify_fresh("codex", "codex", "working_thinking.txt"),
        AgentState::Working,
        Some("codex is thinking about the change"),
    );
}

#[test]
fn codex_idle_at_shell_style_prompt() {
    assert_reading(
        classify_fresh("codex", "codex", "idle_prompt.txt"),
        AgentState::Idle,
        None,
    );
}

#[test]
fn codex_done_shortly_after_activity() {
    // codex sets done.after_activity_secs = 6
    assert_reading(
        classify_after_activity("codex", "codex", "idle_prompt.txt", 3),
        AgentState::Done,
        Some("• Session ready."),
    );
    assert_eq!(
        classify_after_activity("codex", "codex", "idle_prompt.txt", 7).state,
        AgentState::Idle,
    );
}

#[test]
fn aider_blocked_on_yes_no() {
    assert_reading(
        classify_fresh("aider", "aider", "blocked_add_file.txt"),
        AgentState::Blocked,
        Some("Add src/utils/helpers.py to the chat? (Y)es/(N)o [Yes]:"),
    );
}

#[test]
fn aider_blocked_on_add_to_chat() {
    assert_reading(
        classify_fresh("aider", "aider", "blocked_confirm_edit.txt"),
        AgentState::Blocked,
        Some("Add tests/test_app.py to the chat?"),
    );
}

#[test]
fn aider_working_while_applying_edit() {
    assert_reading(
        classify_fresh("aider", "aider", "working_applying.txt"),
        AgentState::Working,
        Some("Applying edit to src/app.py"),
    );
}

#[test]
fn aider_working_while_committing() {
    assert_reading(
        classify_fresh("aider", "aider", "working_committing.txt"),
        AgentState::Working,
        Some("Committing src/app.py with message \"fix: handle empty input\""),
    );
}

#[test]
fn aider_idle_at_bare_prompt() {
    assert_reading(
        classify_fresh("aider", "aider", "idle_prompt.txt"),
        AgentState::Idle,
        None,
    );
}

#[test]
fn aider_done_shortly_after_activity() {
    // aider sets done.after_activity_secs = 5
    assert_eq!(
        classify_after_activity("aider", "aider", "idle_prompt.txt", 2).state,
        AgentState::Done,
    );
    assert_eq!(
        classify_after_activity("aider", "aider", "idle_prompt.txt", 6).state,
        AgentState::Idle,
    );
}

/// The whole per-pane loop over a realistic lifecycle: idle → working →
/// blocked → working → done → idle, at a 1-second cadence, asserting the
/// committed state at every frame — including the debounce lags.
#[test]
fn pane_tracker_full_lifecycle() {
    let detector = Detector::builtin();
    let kind = detector.identify("claude").expect("claude identifies");
    let mut tracker = PaneTracker::new();
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);

    let idle = fixture("claude-code", "idle_prompt.txt");
    let working = fixture("claude-code", "working_esc_hint.txt");
    let blocked = fixture("claude-code", "blocked_proceed.txt");
    let done = fixture("claude-code", "done_after_task.txt");

    // At rest.
    let seen = tracker.update(&detector, kind, &idle, at(0));
    assert_eq!(seen.state, AgentState::Idle);

    // Work starts: one frame of candidate, then committed.
    let seen = tracker.update(&detector, kind, &working, at(1));
    assert_eq!(seen.state, AgentState::Idle);
    let seen = tracker.update(&detector, kind, &working, at(2));
    assert_eq!(seen.state, AgentState::Working);
    assert_eq!(
        seen.reason.as_deref(),
        Some("✳ Compiling roster-core… (esc to interrupt)")
    );

    // A permission prompt appears: blocked commits on the first frame.
    let seen = tracker.update(&detector, kind, &blocked, at(3));
    assert_eq!(seen.state, AgentState::Blocked);
    assert_eq!(seen.reason.as_deref(), Some("Do you want to proceed?"));

    // Approved; work resumes with the usual one-frame lag.
    let seen = tracker.update(&detector, kind, &working, at(4));
    assert_eq!(seen.state, AgentState::Blocked);
    let seen = tracker.update(&detector, kind, &working, at(5));
    assert_eq!(seen.state, AgentState::Working);

    // Output settles into the final screen: the changed frame still reads
    // as working, then the settled prompt reads done and commits.
    let seen = tracker.update(&detector, kind, &done, at(6));
    assert_eq!(seen.state, AgentState::Working);
    let seen = tracker.update(&detector, kind, &done, at(7));
    assert_eq!(seen.state, AgentState::Working);
    let seen = tracker.update(&detector, kind, &done, at(8));
    assert_eq!(seen.state, AgentState::Done);
    assert_eq!(
        seen.reason.as_deref(),
        Some("✓ Done. 3 files changed, 120 insertions.")
    );

    // Long after the done window (8s for claude-code), the pane goes idle.
    let seen = tracker.update(&detector, kind, &done, at(20));
    assert_eq!(seen.state, AgentState::Done);
    let seen = tracker.update(&detector, kind, &done, at(21));
    assert_eq!(seen.state, AgentState::Idle);
    assert_eq!(seen.reason, None);
}
