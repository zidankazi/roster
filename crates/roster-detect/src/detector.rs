//! Agent identification and per-frame state classification.

use std::time::Instant;

use regex::Regex;
use roster_core::{AgentState, Grid};

use crate::config::{parse_agents, AgentConfig, ConfigError, ReasonSource};
use crate::track::History;

/// The default `agents.toml` shipped with roster: Claude Code, Codex, and
/// Aider.
const BUILTIN_AGENTS: &str = include_str!("../agents.toml");

/// One classification result: a state plus the human-readable reason for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateReading {
    /// The classified state.
    pub state: AgentState,
    /// Why — the question a blocked agent is asking, a hint at what a
    /// working agent is doing. `None` when the screen offers nothing usable.
    pub reason: Option<String>,
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

    /// The detector for the shipped default config (Claude Code, Codex,
    /// Aider).
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
    ///    actively moving, even if no pattern shows);
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
            return StateReading {
                state: AgentState::Blocked,
                reason: reason_from(config.reason_blocked, &found, &lines),
            };
        }
        if let Some(found) = find_match(&config.working, &lines) {
            return StateReading {
                state: AgentState::Working,
                reason: reason_from(config.reason_working, &found, &lines),
            };
        }
        if history.content_changed(grid) == Some(true) {
            return StateReading {
                state: AgentState::Working,
                reason: last_worded_line(&lines),
            };
        }
        if let Some(found) = find_match(&config.idle, &lines) {
            let recently_active = history.last_activity_at().is_some_and(|last| {
                at.saturating_duration_since(last) <= config.done_after_activity
            });
            return if recently_active {
                StateReading {
                    state: AgentState::Done,
                    reason: last_worded_line(&lines[..found.row]),
                }
            } else {
                StateReading {
                    state: AgentState::Idle,
                    reason: None,
                }
            };
        }
        StateReading {
            state: AgentState::Idle,
            reason: None,
        }
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

fn reason_from(source: ReasonSource, found: &PatternMatch, lines: &[String]) -> Option<String> {
    match source {
        ReasonSource::MatchedLine => (!found.text.is_empty()).then(|| found.text.clone()),
        ReasonSource::LastNonempty => last_worded_line(lines),
    }
}

/// The bottom-most line containing a word character, cleaned. Lines of pure
/// box-drawing and punctuation are pane chrome, not reasons.
fn last_worded_line(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .find(|line| line.chars().any(char::is_alphanumeric))
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
    fn builtin_config_parses_with_three_agents() {
        let detector = Detector::builtin();
        let names: Vec<&str> = detector.agents().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["aider", "claude-code", "codex"]);
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
        let kind = detector.identify("codex exec 'fix tests'").unwrap();
        assert_eq!(detector.agent(kind).name, "codex");
        let kind = detector.identify("aider --model sonnet").unwrap();
        assert_eq!(detector.agent(kind).name, "aider");
    }

    #[test]
    fn identify_rejects_non_agents() {
        let detector = Detector::builtin();
        assert!(detector.identify("zsh").is_none());
        assert!(detector.identify("/bin/bash -l").is_none());
        assert!(detector.identify("").is_none());
        assert!(detector.identify("claudette").is_none());
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
        history.record(AgentState::Idle, &before, t0);
        let after = Grid::from_text("compiling roster-core v0.1.0\ncompiling roster-detect v0.1.0");
        let reading = detector.classify(kind, &after, &history, t0);
        assert_eq!(reading.state, AgentState::Working);
        assert_eq!(
            reading.reason.as_deref(),
            Some("compiling roster-detect v0.1.0")
        );
    }

    #[test]
    fn static_unrecognized_screen_reads_as_idle() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        let t0 = Instant::now();
        let mut history = History::new();
        let grid = Grid::from_text("plain output\nnothing recognizable");
        history.record(AgentState::Idle, &grid, t0);
        let reading = detector.classify(kind, &grid, &history, t0);
        assert_eq!(reading.state, AgentState::Idle);
        assert_eq!(reading.reason, None);
    }

    #[test]
    fn every_spinner_glyph_reads_as_working() {
        let detector = Detector::builtin();
        let kind = detector.identify("claude").unwrap();
        for glyph in ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] {
            let grid = Grid::from_text(&format!("{glyph} Working on it…"));
            let reading = detector.classify(kind, &grid, &History::new(), Instant::now());
            assert_eq!(reading.state, AgentState::Working, "glyph {glyph}");
        }
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
