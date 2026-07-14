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
fn classify_after_activity(agent: &str, command: &str, name: &str, secs_ago: u64) -> StateReading {
    let detector = Detector::builtin();
    let kind = detector.identify(command).expect("command identifies");
    let grid = fixture(agent, name);
    let t0 = Instant::now();
    let mut history = History::new();
    history.record(AgentState::Working, &grid, detector.agent(kind), t0);
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
    // "esc to interrupt" drives the state; the reason skips that chrome and
    // the status bar to report the spinner status line.
    assert_reading(
        classify_fresh("claude-code", "claude", "working_esc_hint.txt"),
        AgentState::Working,
        Some("✶ Flowing…"),
    );
}

#[test]
fn claude_working_from_ctrl_c_hint() {
    assert_reading(
        classify_fresh("claude-code", "claude", "working_spinner.txt"),
        AgentState::Working,
        Some("⠹ Reticulating…"),
    );
}

#[test]
fn claude_background_wait_stays_working_not_done() {
    // The bug this guards: while waiting on a backgrounded task, no "esc to
    // interrupt" shows and the idle prompt is on screen, so the settled
    // prompt used to read as `done` within the activity window — then flip
    // back to `working` when the task reported. The wait is a working state
    // and must stay working even 3s after the last activity (inside the 8s
    // done window). The reason is the wait line itself; the background-task
    // tray hint below the prompt is skipped as chrome.
    assert_reading(
        classify_after_activity("claude-code", "claude", "working_background_wait.txt", 3),
        AgentState::Working,
        Some("✳ Waiting for 1 background agent to finish"),
    );
}

#[test]
fn claude_background_wait_reads_working_without_history() {
    assert_reading(
        classify_fresh("claude-code", "claude", "working_background_wait.txt"),
        AgentState::Working,
        Some("✳ Waiting for 1 background agent to finish"),
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
fn claude_composing_prompt_reads_idle() {
    // Captured from Claude Code 2.1.205 while typing an unsent prompt: no
    // spinner, no interrupt hint — the agent has done nothing, so the pane
    // is idle (issue #1). A working pattern matching this screen would have
    // no debounce protection: the match is static, so it would never clear.
    assert_reading(
        classify_fresh("claude-code", "claude", "composing_prompt.txt"),
        AgentState::Idle,
        None,
    );
    assert_reading(
        classify_fresh("claude-code", "claude", "composing_prompt_grown.txt"),
        AgentState::Idle,
        None,
    );
}

#[test]
fn claude_working_from_spinner_without_interrupt_hint() {
    // Captured from Claude Code 2.1.205: a genuinely working screen whose
    // only working signal is the spinner status line — no "esc to
    // interrupt" anywhere. The spinner pattern must carry the state on its
    // own, with the spinner line as the reason.
    assert_reading(
        classify_fresh("claude-code", "claude", "working_spinner_only.txt"),
        AgentState::Working,
        Some("✻ Sautéing…"),
    );
    assert_reading(
        classify_fresh("claude-code", "claude", "working_spinner_thinking.txt"),
        AgentState::Working,
        Some("✻ Crunching… (4s · thinking with high effort)"),
    );
    // A second live frame of the same spinner (the glyph rotates), so the
    // class covers more than the one frame a single sample happened to hit.
    assert_reading(
        classify_fresh("claude-code", "claude", "working_spinner_alt_frame.txt"),
        AgentState::Working,
        Some("✢ Sautéing…"),
    );
}

#[test]
fn claude_working_from_multiword_task_status_spinner() {
    // Captured from Claude Code 2.1.207: with an in-progress task the
    // spinner shows the task's live description — several words, not the
    // single random verb — and no interrupt hint. Before the multi-word
    // pattern arm this frame matched nothing, so the settled composer read
    // done and the next status tick flipped back to working: the reported
    // working→done→working flicker mid-task.
    assert_reading(
        classify_fresh("claude-code", "claude", "working_task_status.txt"),
        AgentState::Working,
        Some("✳ Reviewing the roster fixture pipeline… (23s · ↓ 269 tokens)"),
    );
}

#[test]
fn claude_done_shortly_after_spinner_work() {
    // The settled screen from the same 2.1.205 run the spinner fixtures
    // came from: within the done window it reads done, with the response —
    // not the "✻ Brewed for 5s" flourish — as the reason.
    assert_reading(
        classify_after_activity("claude-code", "claude", "done_after_spinner_work.txt", 3),
        AgentState::Done,
        Some("⏺ pumpernickel"),
    );
}

/// The reported repro of issue #1, frame by frame from one live capture:
/// an idle pane, then keystrokes landing in the composer. Only the composer
/// echo and the "● model · /effort" chip change, so the pane stays idle
/// through the typing — and stays idle after it pauses, because composing
/// stamped no activity for the done window to feed on.
#[test]
fn typing_an_unsent_prompt_stays_idle() {
    let detector = Detector::builtin();
    let kind = detector.identify("claude").expect("claude identifies");
    let mut tracker = PaneTracker::new();
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);

    let resting = fixture("claude-code", "idle_placeholder_prompt.txt");
    let typing = fixture("claude-code", "composing_prompt.txt");
    let grown = fixture("claude-code", "composing_prompt_grown.txt");

    let frames: [(u64, &Grid); 6] = [
        (0, &resting),
        (1, &typing),
        (2, &grown),
        // Typing pauses: a false working above would surface a false done
        // here, inside the 8s window.
        (3, &grown),
        (4, &grown),
        (10, &grown),
    ];
    for (secs, grid) in frames {
        let seen = tracker.update(&detector, kind, grid, at(secs));
        assert_eq!(seen.state, AgentState::Idle, "at {secs}s");
        assert_eq!(seen.reason, None, "at {secs}s");
    }
}

/// The wrapped-composer variant of issue #1, frame by frame from one live
/// capture: a prompt long enough to wrap renders continuation rows with no
/// ❯ of their own, and growing it shifts the composer's bounding rules and
/// the status chip. `activity.ignore_region` excludes the whole composer
/// box, so the pane stays idle through all of it — while typing, while the
/// box grows, and after the typing pauses. On a pane whose transcript
/// fills the screen, the box growing a row scrolls the transcript itself,
/// and that shift is indistinguishable from output — but it is a single
/// changed frame per wrap boundary: never enough consecutive frames to
/// commit working, and activity stamps only from the committed state, so
/// it cannot arm the done window either.
#[test]
fn typing_a_wrapped_prompt_stays_idle() {
    let detector = Detector::builtin();
    let kind = detector.identify("claude").expect("claude identifies");
    let mut tracker = PaneTracker::new();
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);

    let wrapped = fixture("claude-code", "composing_wrapped_prompt.txt");
    let grown = fixture("claude-code", "composing_wrapped_grown.txt");

    let frames: [(u64, &Grid); 5] = [
        (0, &wrapped),
        (1, &grown),
        (2, &grown),
        (3, &grown),
        (10, &grown),
    ];
    for (secs, grid) in frames {
        let seen = tracker.update(&detector, kind, grid, at(secs));
        assert_eq!(seen.state, AgentState::Idle, "at {secs}s");
        assert_eq!(seen.reason, None, "at {secs}s");
    }
}

/// The reported "fresh instance defaults to done" bug, frame by frame from
/// a live 2.1.206 spawn capture at the real 400ms detect cadence: the pane
/// is blank until the first paint lands (~1s in), the banner and prompt
/// arrive together and sit quiet, then Claude Code appends its
/// MCP-authentication notice seconds later. None of that is the agent
/// doing work — every frame must read idle, never done and never working.
/// Two guards carry it: blank frames don't settle the change gate, and the
/// late notice is a single changed frame, which never commits working and
/// so never stamps activity for the done window.
#[test]
fn fresh_spawn_startup_chrome_reads_idle_never_done() {
    let detector = Detector::builtin();
    let kind = detector.identify("claude").expect("claude identifies");
    let mut tracker = PaneTracker::new();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let blank = Grid::new(100, 30);
    let banner = fixture("claude-code", "startup_banner.txt");
    let notice = fixture("claude-code", "startup_banner_mcp_notice.txt");

    let frames: [(u64, &Grid); 9] = [
        (400, &blank),
        (800, &blank),
        (1200, &banner),
        (1600, &banner),
        (2000, &banner),
        (2400, &notice),
        (2800, &notice),
        (3200, &notice),
        (11000, &notice),
    ];
    for (ms, grid) in frames {
        let seen = tracker.update(&detector, kind, grid, at(ms));
        assert_eq!(seen.state, AgentState::Idle, "at {ms}ms");
        assert_eq!(seen.reason, None, "at {ms}ms");
    }
}

#[test]
fn claude_wrapped_composing_reads_idle() {
    assert_reading(
        classify_fresh("claude-code", "claude", "composing_wrapped_prompt.txt"),
        AgentState::Idle,
        None,
    );
}

/// The follow-up on issue #1: submit, work, finish — the settled screen
/// must surface done for the done window, not skip straight to idle. All
/// frames are live 2.1.205 captures, where no interrupt hint shows while
/// working.
#[test]
fn submitted_prompt_completing_reads_done_not_idle() {
    let detector = Detector::builtin();
    let kind = detector.identify("claude").expect("claude identifies");
    let mut tracker = PaneTracker::new();
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);

    let composing = fixture("claude-code", "composing_prompt.txt");
    let thinking = fixture("claude-code", "working_spinner_only.txt");
    let responding = fixture("claude-code", "working_spinner_thinking.txt");
    let done = fixture("claude-code", "done_after_spinner_work.txt");

    // Composing: idle (the parent bug would read this as working).
    tracker.update(&detector, kind, &composing, at(0));
    let seen = tracker.update(&detector, kind, &composing, at(1));
    assert_eq!(seen.state, AgentState::Idle);

    // Submitted: the spinner alone reads working, with the usual lag.
    tracker.update(&detector, kind, &thinking, at(2));
    let seen = tracker.update(&detector, kind, &responding, at(3));
    assert_eq!(seen.state, AgentState::Working);

    // Finished: the changed frame still reads working, then the settled
    // prompt commits done — the "just completed, look here" window.
    tracker.update(&detector, kind, &done, at(4));
    tracker.update(&detector, kind, &done, at(5));
    let seen = tracker.update(&detector, kind, &done, at(6));
    assert_eq!(seen.state, AgentState::Done);
    assert_eq!(seen.reason.as_deref(), Some("⏺ pumpernickel"));

    // And past the window it decays to idle.
    let seen = tracker.update(&detector, kind, &done, at(20));
    assert_eq!(seen.state, AgentState::Done);
    let seen = tracker.update(&detector, kind, &done, at(21));
    assert_eq!(seen.state, AgentState::Idle);
    assert_eq!(seen.reason, None);
}

#[test]
fn claude_done_shortly_after_activity() {
    // The completion flourish ("✻ Cogitated for 3s") is ignored chrome;
    // the reason is the last real content line — the response itself.
    assert_reading(
        classify_after_activity("claude-code", "claude", "done_after_task.txt", 3),
        AgentState::Done,
        Some("⏺ pumpernickel"),
    );
}

#[test]
fn claude_done_reason_skips_flourish_and_mode_indicator() {
    // Captured from Claude Code 2.1.204: the flourish sits between the
    // response and the prompt, and "⏸ manual mode on" sits below — both
    // are chrome the reason must skip to land on the response line.
    assert_reading(
        classify_after_activity("claude-code", "claude", "done_flourish_manual_mode.txt", 3),
        AgentState::Done,
        Some("⏺ Hey! 👋  How's it going? What are you working on?"),
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
    assert_eq!(seen.reason.as_deref(), Some("✶ Flowing…"));

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
    // The flourish is ignored chrome; the reason is the response itself.
    assert_eq!(seen.reason.as_deref(), Some("⏺ pumpernickel"));

    // Long after the done window (8s for claude-code), the pane goes idle.
    let seen = tracker.update(&detector, kind, &done, at(20));
    assert_eq!(seen.state, AgentState::Done);
    let seen = tracker.update(&detector, kind, &done, at(21));
    assert_eq!(seen.state, AgentState::Idle);
    assert_eq!(seen.reason, None);
}

/// Blocked commits on the very first reading with no debounce cushion (a
/// false-positive blocked match has no recovery), so every non-blocked
/// fixture is swept here and checked against that no-history path directly,
/// rather than relying on someone remembering a per-fixture assertion.
#[test]
fn working_and_idle_fixtures_never_read_as_blocked() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));

    let mut swept = 0;
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("reading entry in {}: {e}", dir.display()));
        let name = entry.file_name();
        let name = name.to_str().expect("fixture filename is valid utf-8");
        if !name.ends_with(".txt") || name.starts_with("blocked_") {
            continue;
        }

        let reading = classify_fresh("claude-code", "claude", name);
        assert_ne!(
            reading.state,
            AgentState::Blocked,
            "fixture {name} false-blocked"
        );
        swept += 1;
    }

    assert!(swept > 0, "swept zero fixtures — path or filter is wrong");
}
