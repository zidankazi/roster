//! Command-line argument parsing, dependency-free.

use std::path::PathBuf;

/// Parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Path to an `agents.toml` overriding the default lookup.
    pub config: Option<PathBuf>,
    /// One shell command per pane. Empty means a single `$SHELL` pane.
    pub commands: Vec<String>,
    /// Which edge the sidebar occupies, when set on the command line.
    pub sidebar: Option<Side>,
    /// Print usage and exit.
    pub help: bool,
    /// Print the version and exit.
    pub version: bool,
    /// Print the built-in agents.toml and exit.
    pub print_config: bool,
}

/// The sidebar edge requested on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Sidebar on the left (the default).
    Left,
    /// Sidebar on the right.
    Right,
}

/// Usage text for `--help`.
pub const USAGE: &str = "\
roster — agent-aware terminal multiplexer

USAGE:
  roster [OPTIONS] [COMMAND]...

Each COMMAND runs in its own pane (quote multi-word commands). With no
commands, roster opens a single shell pane.

OPTIONS:
  -c, --config <PATH>  Use PATH as agents.toml instead of the default lookup
                       (~/.config/roster/agents.toml, then built-in defaults)
      --sidebar <SIDE> Place the sidebar on the left (default) or right
      --print-config   Print the built-in agents.toml (pipe it to
                       ~/.config/roster/agents.toml to customize)
  -h, --help           Print this help
  -V, --version        Print the version

KEYS (prefix: ctrl-b):
  prefix c   new agent (launcher)      prefix x   close pane
  prefix %   split side by side        prefix \"   split stacked
  prefix o   focus next pane           prefix q   quit
  prefix j   jump via sidebar (j/k move, enter jump, esc cancel)
  prefix ctrl-b  send a literal ctrl-b

MOUSE:
  click a pane or its title to focus it; click a sidebar card to jump to
  that agent; click launcher rows to launch; drag the divider between
  panes to resize; scroll to nudge the pane under the cursor.
";

/// Parse arguments (excluding argv\[0\]).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut iter = args.into_iter();
    let mut positional_only = false;
    while let Some(arg) = iter.next() {
        if positional_only {
            parsed.commands.push(arg);
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => parsed.help = true,
            "-V" | "--version" => parsed.version = true,
            "--print-config" => parsed.print_config = true,
            "-c" | "--config" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path"))?;
                parsed.config = Some(PathBuf::from(value));
            }
            "--sidebar" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires left or right"))?;
                parsed.sidebar = Some(match value.as_str() {
                    "left" => Side::Left,
                    "right" => Side::Right,
                    other => return Err(format!("--sidebar expects left or right, got {other}")),
                });
            }
            "--" => positional_only = true,
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option: {flag}"));
            }
            _ => parsed.commands.push(arg),
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn commands_are_positional() {
        let args = parse(strings(&["claude", "codex exec 'fix'"])).unwrap();
        assert_eq!(args.commands, vec!["claude", "codex exec 'fix'"]);
        assert!(!args.help);
    }

    #[test]
    fn config_takes_a_value() {
        let args = parse(strings(&["-c", "custom.toml", "claude"])).unwrap();
        assert_eq!(args.config, Some(PathBuf::from("custom.toml")));
        assert_eq!(args.commands, vec!["claude"]);
    }

    #[test]
    fn config_without_value_errors() {
        assert!(parse(strings(&["--config"])).is_err());
    }

    #[test]
    fn unknown_flags_error() {
        assert!(parse(strings(&["--bogus"])).is_err());
    }

    #[test]
    fn double_dash_ends_flags() {
        let args = parse(strings(&["--", "--help"])).unwrap();
        assert!(!args.help);
        assert_eq!(args.commands, vec!["--help"]);
    }

    #[test]
    fn help_and_version_flags() {
        assert!(parse(strings(&["-h"])).unwrap().help);
        assert!(parse(strings(&["--version"])).unwrap().version);
    }

    #[test]
    fn sidebar_side_parses() {
        assert_eq!(parse(strings(&[])).unwrap().sidebar, None);
        assert_eq!(
            parse(strings(&["--sidebar", "right"])).unwrap().sidebar,
            Some(Side::Right)
        );
        assert_eq!(
            parse(strings(&["--sidebar", "left", "claude"]))
                .unwrap()
                .sidebar,
            Some(Side::Left)
        );
        assert!(parse(strings(&["--sidebar", "top"])).is_err());
        assert!(parse(strings(&["--sidebar"])).is_err());
    }
}
