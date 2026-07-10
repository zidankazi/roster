//! Agent identification and per-frame state classification.

use std::time::Instant;

use regex::Regex;
use roster_core::{AgentState, Grid, Telemetry};

use crate::config::{parse_agents, AgentConfig, ConfigError, ReasonSource};
use crate::track::History;

/// The default `agents.toml` shipped with roster: Claude Code only.
const BUILTIN_AGENTS: &str = include_str!("../agents.toml");

/// One classification result: a state plus the human-readable reason for it.
///
/// Not `Eq`: telemetry carries `f32` readings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateReading {
    /// The classified state.
    pub state: AgentState,
    /// Why — the question a blocked agent is asking, a hint at what a
    /// working agent is doing. `None` when the screen offers nothing usable.
    pub reason: Option<String>,
    /// Statusline-fed numbers for the pane, when its bridge feed is live.
    /// Never scraped: [`Detector::classify`] always leaves it `None`;
    /// [`crate::PaneTracker`] attaches and ages it, so scraping-only panes
    /// are untouched by its existence.
    pub telemetry: Option<Telemetry>,
}

/// Identifies a configured agent within a [`Detector`].
///
/// Obtained from [`Detector::identify`]; only meaningful for the detector
/// that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentKind(usize);

/// Classifies agent panes from parsed screen grids.
pub struct Detector {
    agents: Vec<AgentConfig>,
}

impl Detector {
    /// A detector over the given agent configs.
    pub fn new(agents: Vec<AgentConfig>) -> Self {
        Detector { agents }
    }

    /// Parse an `agents.toml` document and build a detector from it.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        Ok(Detector::new(parse_agents(text)?))
    }

    /// The detector for the shipped default config (Claude Code only).
    pub fn builtin() -> Self {
        Detector::from_toml(BUILTIN_AGENTS).expect("embedded agents.toml is valid")
    }

    /// The shipped default `agents.toml`, verbatim — a starting point for a
    /// user's own config.
    pub fn builtin_toml() -> &'static str {
        BUILTIN_AGENTS
    }

    /// The configured agents, in name order.
    pub fn agents(&self) -> impl Iterator<Item = &AgentConfig> {
        self.agents.iter()
    }

    /// The config behind an [`AgentKind`].
    pub fn agent(&self, kind: AgentKind) -> &AgentConfig {
        &self.agents[kind.0]
    }

    /// Match a pane's command line against the configured agents.
    ///
    /// The first whitespace-separated token is compared by basename, so
    /// `/opt/homebrew/bin/claude --continue` identifies as `claude`. Walking
    /// the process tree when the direct command is a shell is the binary's
    /// job — this stays pure string matching.
    pub fn identify(&self, command: &str) -> Option<AgentKind> {
        let first = command.split_whitespace().next()?;
        let base = first.rsplit('/').next().unwrap_or(first);
        self.agents
            .iter()
            .position(|agent| agent.match_command.iter().any(|m| m == base))
            .map(AgentKind)
    }

    /// Classify one frame of an agent's screen.
    ///
    /// Signals, strongest first:
    /// 1. a blocked pattern on screen — the agent needs the human;
    /// 2. a working pattern on screen;
    /// 3. screen content changed since the last recorded frame (output is
    ///    actively moving, even if no pattern shows) — but only once the
    ///    screen has settled at least once ([`History::has_settled`]):
    ///    before that, changes are the program painting its initial UI
    ///    (blank frames never settle — nothing painted proves nothing),
    ///    and counting them would read a freshly spawned agent as done
    ///    the moment its prompt appears;
    /// 4. an idle-prompt pattern — read as `done` when the agent was working
    ///    within its `done.after_activity_secs` window, `idle` otherwise;
    /// 5. nothing recognizable — `idle`.
    ///
    /// Rows are matched bottom-up (prompts live at the bottom); within a
    /// state, patterns are tried in config order, so earlier patterns both
    /// win the match and supply the reason. `at` is the frame's timestamp
    /// and only feeds the done-vs-idle recency call — pass the same clock
    /// you record history with.
    pub fn classify(
        &self,
        kind: AgentKind,
        grid: &Grid,
        history: &History,
        at: Instant,
    ) -> StateReading {
        let config = self.agent(kind);
        let lines = grid.lines();

        if let Some(found) = find_match(&config.blocked, &lines) {
            return scraped(
                AgentState::Blocked,
                reason_from(config.reason_blocked, &found, &lines, &config.reason_ignore),
            );
        }
        if let Some(found) = find_match(&config.working, &lines) {
            return scraped(
                AgentState::Working,
                reason_from(config.reason_working, &found, &lines, &config.reason_ignore),
            );
        }
        if history.has_settled() && history.content_changed(grid, config) == Some(true) {
            return scraped(
                AgentState::Working,
                last_worded_line(&lines, &config.reason_ignore),
            );
        }
        if let Some(found) = find_match(&config.idle, &lines) {
            let recently_active = history.last_activity_at().is_some_and(|last| {
                at.saturating_duration_since(last) <= config.done_after_activity
            });
            return if recently_active {
                scraped(
                    AgentState::Done,
                    last_worded_line(&lines[..found.row], &config.reason_ignore),
                )
            } else {
                scraped(AgentState::Idle, None)
            };
        }
        scraped(AgentState::Idle, None)
    }
}

/// A reading as the scrape produces it: state and reason only. The single
/// chokepoint for the invariant that bridge-sourced fields (telemetry) are
/// never set from a screen — [`crate::PaneTracker`] attaches those.
fn scraped(state: AgentState, reason: Option<String>) -> StateReading {
    StateReading {
        state,
        reason,
        telemetry: None,
    }
}

struct PatternMatch {
    /// Row index of the matched line.
    row: usize,
    /// The reason candidate: capture group 1 when present, else the whole
    /// matched line, cleaned of pane chrome.
    text: String,
}

/// Try `patterns` in order; for each, scan rows bottom-up. First hit wins.
fn find_match(patterns: &[Regex], lines: &[String]) -> Option<PatternMatch> {
    for pattern in patterns {
        for (row, line) in lines.iter().enumerate().rev() {
            if let Some(captures) = pattern.captures(line) {
                let text = captures.get(1).map(|group| group.as_str()).unwrap_or(line);
                return Some(PatternMatch {
                    row,
                    text: clean_line(text),
                });
            }
        }
    }
    None
}

fn reason_from(
    source: ReasonSource,
    found: &PatternMatch,
    lines: &[String],
    ignore: &[Regex],
) -> Option<String> {
    match source {
        ReasonSource::MatchedLine => (!found.text.is_empty()).then(|| found.text.clone()),
        ReasonSource::LastNonempty => last_worded_line(lines, ignore),
    }
}

/// The bottom-most content line, cleaned. Lines of pure box-drawing and
/// punctuation are skipped (pane chrome), as are lines matching any of the
/// agent's `reason.ignore` patterns (status bars, interrupt hints).
fn last_worded_line(lines: &[String], ignore: &[Regex]) -> Option<String> {
    lines
        .iter()
        .rev()
        .filter(|line| line.chars().any(char::is_alphanumeric))
        .find(|line| !ignore.iter().any(|pattern| pattern.is_match(line)))
        .map(|line| clean_line(line))
}

/// Strip surrounding whitespace and box borders so a reason reads as text,
/// not as a slice of the UI.
fn clean_line(line: &str) -> String {
    line.trim_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '┃' | '║'))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_config_parses_with_only_claude_code() {
        let detector = Detector::builtin();
        let names: Vec<&str> = detector.agents().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["claude-code"]);
    }

    #[test]
    fn identify_matches_bare_command() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        assert_eq!(detector.agent(kind).name, "claude-code");
    }

    #[test]
    fn identify_matches_path_and_arguments() {
        let detector = Detector::builtin();
        let kind = detector
            .identify("/opt/homebrew/bin/claude --continue")
            .unwrap();
        assert_eq!(detector.agent(kind).name, "claude-code");
    }

    #[test]
    fn identify_rejects_non_agents() {
        let detector = Detector::builtin();
        assert!(detector.identify("zsh").is_none());
        assert!(detector.identify("/bin/bash -l").is_none());
        assert!(detector.identify("").is_none());
        assert!(detector.identify("claudette").is_none());
        // roster is Claude-exclusive: other agent CLIs are not identified.
        assert!(detector.identify("codex exec 'fix tests'").is_none());
        assert!(detector.identify("aider --model sonnet").is_none());
    }

    #[test]
    fn capture_group_narrows_the_reason() {
        let detector = Detector::from_toml(
            r#"
            [test-agent]
            match_command = ["ta"]
            blocked = ['Allow (.*)\?']
            "#,
        )
        .unwrap();
        let kind = detector.identify("ta").unwrap();
        let grid = Grid::from_text("Allow edit to src/config.ts?");
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.state, AgentState::Blocked);
        assert_eq!(reading.reason.as_deref(), Some("edit to src/config.ts"));
    }

    #[test]
    fn bottom_row_wins_within_one_pattern() {
        let detector = Detector::from_toml(
            r#"
            [test-agent]
            match_command = ["ta"]
            blocked = ['Allow .*\?']
            "#,
        )
        .unwrap();
        let kind = detector.identify("ta").unwrap();
        let grid = Grid::from_text("Allow read?\nsome output\nAllow write?");
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.reason.as_deref(), Some("Allow write?"));
    }

    #[test]
    fn earlier_pattern_outranks_lower_row() {
        // "Do you want to proceed?" is listed before the ❯-menu pattern, so
        // it supplies the reason even though the menu row sits below it.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let grid = Grid::from_text("│ Do you want to proceed?\n│ ❯ 1. Yes\n│   2. No");
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.state, AgentState::Blocked);
        assert_eq!(reading.reason.as_deref(), Some("Do you want to proceed?"));
    }

    #[test]
    fn content_change_reads_as_working() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let mut history = History::new();
        let before = Grid::from_text("compiling roster-core v0.1.0");
        // Recorded twice: the change gate opens only once the screen has
        // settled (see startup_paint_never_reads_done_or_working).
        history.record(AgentState::Idle, &before, detector.agent(kind), t0);
        history.record(AgentState::Idle, &before, detector.agent(kind), t0);
        let after = Grid::from_text("compiling roster-core v0.1.0\ncompiling roster-detect v0.1.0");
        let reading = detector.classify(kind, &after, &history, t0);
        assert_eq!(reading.state, AgentState::Working);
        assert_eq!(
            reading.reason.as_deref(),
            Some("compiling roster-detect v0.1.0")
        );
    }

    #[test]
    fn startup_paint_never_reads_done_or_working() {
        // A freshly spawned agent paints its banner over several frames,
        // then shows its prompt. Every frame differs from the last, but
        // none of that is task activity: with the settle gate, no frame
        // reads working, no activity is stamped, and the prompt reads idle
        // — not done. Without the gate, the paint read as working and the
        // prompt that followed landed inside the done window.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
        let mut history = History::new();
        // The blank polls before the child's first output must not settle
        // the gate — matching blank frames prove nothing painted, not that
        // the screen held still.
        let paint = [
            "",
            "",
            "✻ Welcome to Claude Code",
            "✻ Welcome to Claude Code\n\n  /help for help, /status for your current setup",
            "✻ Welcome to Claude Code\n\n  /help for help, /status for your current setup\n\n❯ Try \"fix lint errors\"",
        ];
        for (i, text) in paint.iter().enumerate() {
            let grid = Grid::from_text(text);
            let reading = detector.classify(kind, &grid, &history, at(i as u64));
            assert_eq!(reading.state, AgentState::Idle, "paint frame {i}");
            history.record(reading.state, &grid, detector.agent(kind), at(i as u64));
        }
        // The prompt holds still: settled now, and still idle — the paint
        // stamped no activity for the done window to feed on.
        let settled = Grid::from_text(paint[4]);
        let reading = detector.classify(kind, &settled, &history, at(5));
        assert_eq!(reading.state, AgentState::Idle);
        history.record(reading.state, &settled, detector.agent(kind), at(5));
        assert_eq!(history.last_activity_at(), None);

        // The gate is open after the settle: output moving reads working
        // again, so a real task's completion still reads done.
        let output = Grid::from_text("❯ fix the tests\nrunning cargo test");
        let reading = detector.classify(kind, &output, &history, at(6));
        assert_eq!(reading.state, AgentState::Working);
        history.record(reading.state, &output, detector.agent(kind), at(6));
        let finished = Grid::from_text("⏺ all tests pass\n❯");
        let reading = detector.classify(kind, &finished, &history, at(7));
        assert_eq!(reading.state, AgentState::Working, "still-moving frame");
        history.record(reading.state, &finished, detector.agent(kind), at(7));
        let reading = detector.classify(kind, &finished, &history, at(8));
        assert_eq!(reading.state, AgentState::Done);
    }

    #[test]
    fn static_unrecognized_screen_reads_as_idle() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let mut history = History::new();
        let grid = Grid::from_text("plain output\nnothing recognizable");
        history.record(AgentState::Idle, &grid, detector.agent(kind), t0);
        let reading = detector.classify(kind, &grid, &history, t0);
        assert_eq!(reading.state, AgentState::Idle);
        assert_eq!(reading.reason, None);
    }

    #[test]
    fn interrupt_hints_read_as_working() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        for hint in [
            "esc to interrupt",
            "ctrl+c to interrupt",
            "Ctrl+C to interrupt",
        ] {
            let grid = Grid::from_text(&format!("✶ Flowing…\n{hint}"));
            let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
            assert_eq!(reading.state, AgentState::Working, "hint {hint}");
        }
    }

    #[test]
    fn spinner_lookalike_bullets_do_not_read_working() {
        // A settled response containing a bullet or quoted spinner-shaped
        // text must not match the spinner working pattern: a static false
        // working never clears. The flower glyphs are reserved enough to
        // risk; '*' and '·' bullets are not, so they stay out of the class.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        for line in ["* Loading… see the logs", "  * Retrying…", "· Updating…"] {
            let grid = Grid::from_text(&format!("⏺ answer\n{line}\n❯"));
            let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
            assert_eq!(reading.state, AgentState::Idle, "line {line}");
        }
    }

    #[test]
    fn task_header_rows_still_count_as_activity() {
        // activity.ignore's chip pattern requires indentation: flush-left
        // "● Task(…)" rows are real output, and their changes must keep
        // feeding the change fingerprint even though the right-aligned
        // "● high · /effort" chip is excluded.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let mut history = History::new();
        let before = Grid::from_text("● Explore(map the sidebar)\n❯");
        let after = Grid::from_text("● Explore(map the sidebar, done)\n❯");
        // Recorded twice: the change gate needs a settled screen first.
        history.record(AgentState::Idle, &before, detector.agent(kind), t0);
        history.record(AgentState::Idle, &before, detector.agent(kind), t0);
        let reading = detector.classify(kind, &after, &history, t0);
        assert_eq!(reading.state, AgentState::Working);
    }

    #[test]
    fn tray_progress_below_the_composer_counts_as_activity() {
        // The background-task tray sits BELOW the composer box (layout as
        // captured in working_background_wait.txt): the ignore_region must
        // end at the box's closing rule so the tray's ticking progress row
        // keeps feeding the change fingerprint. Only the composer is the
        // human's surface; the tray is the agent's.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let mut history = History::new();
        let screen = |uses: u32| {
            Grid::from_text(&format!(
                "⏺ kicked off the mapping\n\
                 ────────\n\
                 ❯\n\
                 ────────\n\
                 \x20\x20Enter to view · x to stop\n\
                 ● Explore(mapping · {uses} tool uses)"
            ))
        };
        // Recorded twice: the change gate needs a settled screen first.
        history.record(AgentState::Idle, &screen(3), detector.agent(kind), t0);
        history.record(AgentState::Idle, &screen(3), detector.agent(kind), t0);
        let reading = detector.classify(kind, &screen(4), &history, t0);
        assert_eq!(reading.state, AgentState::Working);
    }

    #[test]
    fn working_reason_skips_interrupt_and_status_chrome() {
        // The reason should be the spinner status line, not the model status
        // bar (`● …/effort`), the input prompt (`❯`), or the interrupt hint.
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let grid = Grid::from_text(
            "❯ do the thing\n✶ Flowing…\n                    ● high · /effort\n❯\n  esc to interrupt",
        );
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.state, AgentState::Working);
        assert_eq!(reading.reason.as_deref(), Some("✶ Flowing…"));
    }

    #[test]
    fn empty_and_blank_grids_read_as_idle() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        for grid in [
            Grid::new(0, 0),
            Grid::new(80, 24),
            Grid::from_text("\n\n\n"),
        ] {
            let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
            assert_eq!(reading.state, AgentState::Idle);
            assert_eq!(reading.reason, None);
        }
    }

    #[test]
    fn blocked_prompt_on_the_last_row_is_found() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let grid = Grid::from_text("output\nmore output\nAllow rm -rf target/?");
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.state, AgentState::Blocked);
        assert_eq!(reading.reason.as_deref(), Some("Allow rm -rf target/?"));
    }

    #[test]
    fn first_frame_is_not_evidence_of_activity() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let grid = Grid::from_text("some startup banner");
        let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
        assert_eq!(reading.state, AgentState::Idle);
    }
}
