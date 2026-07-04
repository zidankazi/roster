//! roster — agent-aware terminal multiplexer.
//!
//! Wires the crates together: `roster-pty` spawns agents, `roster-term`
//! parses their output, `roster-detect` classifies each screen,
//! `roster-core` holds the model, `roster-tui` paints it. This binary owns
//! the event loop and all side effects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use roster_detect::Detector;
use roster_tui::SidebarSide;

mod app;
mod cli;
mod keys;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("roster: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = cli::parse(std::env::args().skip(1))?;
    if args.help {
        print!("{}", cli::USAGE);
        return Ok(());
    }
    if args.version {
        println!("roster {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.print_config {
        print!("{}", Detector::builtin_toml());
        return Ok(());
    }

    let detector = load_detector(args.config.as_deref())?;
    let commands = if args.commands.is_empty() {
        vec![app::default_shell()]
    } else {
        args.commands
    };
    let side = match args.sidebar {
        Some(cli::Side::Right) => SidebarSide::Right,
        Some(cli::Side::Left) | None => SidebarSide::Left,
    };

    let mut app = app::App::new(detector, &commands, side)?;
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result.map_err(|e| e.to_string())
}

/// Load detection config: an explicit `--config` path must parse; the
/// default location is used when present; otherwise the built-in agents
/// ship with the binary.
fn load_detector(config: Option<&Path>) -> Result<Detector, String> {
    let path = match config {
        Some(path) => path.to_path_buf(),
        None => {
            let Some(path) = default_config_path() else {
                return Ok(Detector::builtin());
            };
            if !path.exists() {
                return Ok(Detector::builtin());
            }
            path
        }
    };
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Detector::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `$XDG_CONFIG_HOME/roster/agents.toml`, defaulting `$XDG_CONFIG_HOME` to
/// `~/.config`.
fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("roster").join("agents.toml"))
}
