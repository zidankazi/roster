//! Loading and validating the declarative `agents.toml`.
//!
//! New agents are added as data, not code: each entry names the binaries
//! that identify the agent and the regex patterns that classify its screen.
//! Parsing is strict — unknown keys, bad regexes, and unknown reason sources
//! are errors, so a config typo fails loudly instead of silently never
//! matching.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;

/// Fallback for `done.after_activity_secs` when an agent doesn't set it.
/// Tuned against the observed gap between Claude Code's completion
/// flourish and its next idle prompt.
const DEFAULT_DONE_AFTER_SECS: u64 = 8;

/// Where a state's human-readable reason is pulled from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasonSource {
    /// The row that matched the pattern — or the pattern's first capture
    /// group, when it has one.
    #[default]
    MatchedLine,
    /// The bottom-most row containing a word character. Rows that are only
    /// box-drawing and punctuation (pane chrome) don't count.
    LastNonempty,
}

impl ReasonSource {
    fn parse(agent: &str, value: &str) -> Result<Self, ConfigError> {
        match value {
            "matched_line" => Ok(ReasonSource::MatchedLine),
            "last_nonempty" => Ok(ReasonSource::LastNonempty),
            _ => Err(ConfigError::ReasonSource {
                agent: agent.to_string(),
                value: value.to_string(),
            }),
        }
    }
}

/// One agent's compiled detection rules.
#[derive(Debug)]
pub struct AgentConfig {
    /// The agent's config key, e.g. `claude-code`.
    pub name: String,
    /// Binary names whose panes belong to this agent.
    pub match_command: Vec<String>,
    /// The full command the launcher starts this agent with — flags
    /// included. `None` falls back to the first `match_command` binary.
    pub launch_command: Option<String>,
    /// Patterns meaning "blocked on input", in priority order.
    pub blocked: Vec<Regex>,
    /// Patterns meaning "actively working", in priority order.
    pub working: Vec<Regex>,
    /// Patterns matching the agent's at-rest prompt, in priority order.
    pub idle: Vec<Regex>,
    /// Reason source for `blocked` readings.
    pub reason_blocked: ReasonSource,
    /// Reason source for `working` readings.
    pub reason_working: ReasonSource,
    /// Lines matching any of these are treated as UI chrome (status bars,
    /// interrupt hints, shortcut legends) and skipped when a `last_nonempty`
    /// reason is chosen, so the reason is real content rather than framing.
    pub reason_ignore: Vec<Regex>,
    /// Lines matching any of these are excluded from the change fingerprint
    /// that reads "screen moved" as working: rows that change without the
    /// agent doing anything — the composer echoing keystrokes, a status bar
    /// toggling — must not count as activity.
    pub activity_ignore: Vec<Regex>,
    /// An idle prompt appearing within this window after activity reads as
    /// `done` rather than `idle`.
    pub done_after_activity: Duration,
}

/// Why a config failed to load.
#[derive(Debug)]
pub enum ConfigError {
    /// The TOML itself didn't parse or didn't fit the expected shape.
    Toml(toml::de::Error),
    /// A state pattern isn't a valid regular expression.
    Pattern {
        /// Agent the pattern belongs to.
        agent: String,
        /// The offending pattern.
        pattern: String,
        /// The regex compiler's complaint.
        error: regex::Error,
    },
    /// A `reason.*` value names an unknown source.
    ReasonSource {
        /// Agent the value belongs to.
        agent: String,
        /// The unknown value.
        value: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Toml(e) => write!(f, "invalid agents config: {e}"),
            ConfigError::Pattern {
                agent,
                pattern,
                error,
            } => write!(f, "agent `{agent}`: invalid pattern `{pattern}`: {error}"),
            ConfigError::ReasonSource { agent, value } => write!(
                f,
                "agent `{agent}`: unknown reason source `{value}` \
                 (expected `matched_line` or `last_nonempty`)"
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Toml(e) => Some(e),
            ConfigError::Pattern { error, .. } => Some(error),
            ConfigError::ReasonSource { .. } => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    match_command: Vec<String>,
    #[serde(default)]
    launch_command: Option<String>,
    #[serde(default)]
    blocked: Vec<String>,
    #[serde(default)]
    working: Vec<String>,
    #[serde(default)]
    idle: Vec<String>,
    #[serde(default)]
    reason: RawReason,
    #[serde(default)]
    activity: RawActivity,
    #[serde(default)]
    done: RawDone,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawReason {
    blocked: Option<String>,
    working: Option<String>,
    #[serde(default)]
    ignore: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawActivity {
    #[serde(default)]
    ignore: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDone {
    after_activity_secs: Option<u64>,
}

/// Parse an `agents.toml` document into compiled agent configs, sorted by
/// agent name.
pub fn parse_agents(text: &str) -> Result<Vec<AgentConfig>, ConfigError> {
    let raw: BTreeMap<String, RawAgent> = toml::from_str(text).map_err(ConfigError::Toml)?;
    raw.into_iter()
        .map(|(name, agent)| compile_agent(name, agent))
        .collect()
}

fn compile_agent(name: String, raw: RawAgent) -> Result<AgentConfig, ConfigError> {
    let reason_blocked = match &raw.reason.blocked {
        Some(value) => ReasonSource::parse(&name, value)?,
        None => ReasonSource::MatchedLine,
    };
    let reason_working = match &raw.reason.working {
        Some(value) => ReasonSource::parse(&name, value)?,
        None => ReasonSource::LastNonempty,
    };
    Ok(AgentConfig {
        blocked: compile_patterns(&name, raw.blocked)?,
        working: compile_patterns(&name, raw.working)?,
        idle: compile_patterns(&name, raw.idle)?,
        reason_ignore: compile_patterns(&name, raw.reason.ignore)?,
        activity_ignore: compile_patterns(&name, raw.activity.ignore)?,
        match_command: raw.match_command,
        launch_command: raw.launch_command,
        reason_blocked,
        reason_working,
        done_after_activity: Duration::from_secs(
            raw.done
                .after_activity_secs
                .unwrap_or(DEFAULT_DONE_AFTER_SECS),
        ),
        name,
    })
}

fn compile_patterns(agent: &str, patterns: Vec<String>) -> Result<Vec<Regex>, ConfigError> {
    patterns
        .into_iter()
        .map(|pattern| {
            Regex::new(&pattern).map_err(|error| ConfigError::Pattern {
                agent: agent.to_string(),
                pattern,
                error,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_agent_gets_defaults() {
        let agents = parse_agents(
            r#"
            [minimal]
            match_command = ["mini"]
            "#,
        )
        .unwrap();
        let agent = &agents[0];
        assert_eq!(agent.name, "minimal");
        assert_eq!(agent.reason_blocked, ReasonSource::MatchedLine);
        assert_eq!(agent.reason_working, ReasonSource::LastNonempty);
        assert_eq!(
            agent.done_after_activity,
            Duration::from_secs(DEFAULT_DONE_AFTER_SECS)
        );
        assert!(agent.blocked.is_empty());
        assert_eq!(agent.launch_command, None);
    }

    #[test]
    fn launch_command_parses_with_flags() {
        let agents = parse_agents(
            r#"
            [claude-code]
            match_command = ["claude"]
            launch_command = "claude --dangerously-skip-permissions"
            "#,
        )
        .unwrap();
        assert_eq!(
            agents[0].launch_command.as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
    }

    #[test]
    fn agents_come_back_sorted_by_name() {
        let agents = parse_agents(
            r#"
            [zeta]
            match_command = ["z"]
            [alpha]
            match_command = ["a"]
            "#,
        )
        .unwrap();
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn bad_regex_is_reported_with_agent_and_pattern() {
        let err = parse_agents(
            r#"
            [broken]
            match_command = ["b"]
            blocked = ['(unclosed']
            "#,
        )
        .unwrap_err();
        match err {
            ConfigError::Pattern { agent, pattern, .. } => {
                assert_eq!(agent, "broken");
                assert_eq!(pattern, "(unclosed");
            }
            other => panic!("expected Pattern error, got {other}"),
        }
    }

    #[test]
    fn unknown_reason_source_is_an_error() {
        let err = parse_agents(
            r#"
            [odd]
            match_command = ["o"]
            reason.blocked = "first_line"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::ReasonSource { .. }));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = parse_agents(
            r#"
            [typo]
            match_command = ["t"]
            blockde = ['x']
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Toml(_)));
    }

    #[test]
    fn missing_match_command_is_an_error() {
        let err = parse_agents(
            r#"
            [nocmd]
            blocked = ['x']
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Toml(_)));
    }
}
