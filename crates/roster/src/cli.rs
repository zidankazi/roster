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
    /// Print the Claude Code hooks snippet for `~/.claude/settings.json`
    /// and exit.
    pub print_hooks: bool,
    /// Run inside the named persistent session (create it if needed).
    pub session: Option<String>,
    /// A subcommand, when the first positional was one.
    pub action: Option<Action>,
}

/// Session-management subcommands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Attach to an existing session: `roster attach <name>`. The name may
    /// be `user@host:name` for a remote session over ssh.
    Attach(String),
    /// List sessions: `roster ls`.
    List,
    /// Kill a session: `roster kill <name>`.
    Kill(String),
    /// Hidden: run the session server for `<name>`.
    Server(String),
    /// Hidden: bridge stdio to a local session socket (the remote half of
    /// ssh attach).
    Proxy(String),
    /// Hidden: forward a Claude Code hook payload (stdin JSON) to the pane's
    /// roster instance. Registered via `--print-hooks`; a no-op outside a
    /// roster pane.
    Hook,
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
  roster attach <NAME>       attach to a persistent session
  roster attach <USER@HOST:NAME>  attach to a session over ssh
  roster ls                  list persistent sessions
  roster kill <NAME>         kill a persistent session and its agents

Each COMMAND runs in its own pane (quote multi-word commands). With no
commands, roster opens a single shell pane.

OPTIONS:
  -s, --session <NAME> Run inside the persistent session NAME: agents keep
                       running after roster exits; reattach with
                       `roster attach NAME` (created on first use)
  -c, --config <PATH>  Use PATH as agents.toml instead of the default lookup
                       (~/.config/roster/agents.toml, then built-in defaults)
      --sidebar <SIDE> Place the sidebar on the left (default) or right
      --print-config   Print the built-in agents.toml (pipe it to
                       ~/.config/roster/agents.toml to customize)
      --print-hooks    Print the Claude Code hooks that report exact agent
                       state to roster (merge into ~/.claude/settings.json)
  -h, --help           Print this help
  -V, --version        Print the version

KEYS (prefix: ctrl-b):
  prefix c   new agent (launcher)      prefix x   close pane
  prefix %   split side by side        prefix \"   split stacked
  prefix o   focus next pane           prefix n/p next/previous window
  prefix ,   rename workspace (empty input restores the automatic name)
  prefix j   jump via sidebar (j/k move, enter jump, esc cancel)
  prefix d   detach (persistent sessions)
  prefix q   quit
  prefix ctrl-b  send a literal ctrl-b

MOUSE:
  click a pane or its title to focus it; click a sidebar card or workspace
  header to jump; click launcher rows to launch; drag the divider between
  panes to resize; wheel-scroll a pane's history; drag over text to select
  and copy it.
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
            "--print-hooks" => parsed.print_hooks = true,
            "-c" | "--config" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path"))?;
                parsed.config = Some(PathBuf::from(value));
            }
            "-s" | "--session" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a session name"))?;
                parsed.session = Some(value);
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
            // Session subcommands claim the first positional slot; `--`
            // still lets a command literally named `attach` through.
            "attach" if parsed.commands.is_empty() && parsed.action.is_none() => {
                let name = iter.next().ok_or("attach requires a session name")?;
                parsed.action = Some(Action::Attach(name));
            }
            "ls" if parsed.commands.is_empty() && parsed.action.is_none() => {
                parsed.action = Some(Action::List);
            }
            "kill" if parsed.commands.is_empty() && parsed.action.is_none() => {
                let name = iter.next().ok_or("kill requires a session name")?;
                parsed.action = Some(Action::Kill(name));
            }
            "_server" if parsed.commands.is_empty() && parsed.action.is_none() => {
                let name = iter.next().ok_or("_server requires a session name")?;
                parsed.action = Some(Action::Server(name));
            }
            "_proxy" if parsed.commands.is_empty() && parsed.action.is_none() => {
                let name = iter.next().ok_or("_proxy requires a session name")?;
                parsed.action = Some(Action::Proxy(name));
            }
            "_hook" if parsed.commands.is_empty() && parsed.action.is_none() => {
                parsed.action = Some(Action::Hook);
            }
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
        let args = parse(strings(&["claude", "claude --continue"])).unwrap();
        assert_eq!(args.commands, vec!["claude", "claude --continue"]);
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
    fn session_flag_and_subcommands_parse() {
        let args = parse(strings(&["-s", "work", "claude"])).unwrap();
        assert_eq!(args.session.as_deref(), Some("work"));
        assert_eq!(args.commands, vec!["claude"]);

        assert_eq!(
            parse(strings(&["attach", "work"])).unwrap().action,
            Some(Action::Attach("work".into()))
        );
        assert_eq!(parse(strings(&["ls"])).unwrap().action, Some(Action::List));
        assert_eq!(
            parse(strings(&["kill", "work"])).unwrap().action,
            Some(Action::Kill("work".into()))
        );
        assert_eq!(
            parse(strings(&["_server", "work"])).unwrap().action,
            Some(Action::Server("work".into()))
        );
        assert_eq!(
            parse(strings(&["_hook"])).unwrap().action,
            Some(Action::Hook)
        );
        assert!(parse(strings(&["--print-hooks"])).unwrap().print_hooks);
        assert!(parse(strings(&["attach"])).is_err());

        // Subcommands only claim the first positional; later words are
        // commands, and `--` forces even the first through.
        let args = parse(strings(&["claude", "ls"])).unwrap();
        assert_eq!(args.action, None);
        assert_eq!(args.commands, vec!["claude", "ls"]);
        let args = parse(strings(&["--", "ls"])).unwrap();
        assert_eq!(args.action, None);
        assert_eq!(args.commands, vec!["ls"]);
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
