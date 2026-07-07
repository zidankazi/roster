//! roster — agent-aware terminal multiplexer.
//!
//! Wires the crates together: `roster-pty` spawns agents, `roster-term`
//! parses their output, `roster-detect` classifies each screen,
//! `roster-core` holds the model, `roster-tui` paints it. This binary owns
//! the event loop and all side effects — including the session server
//! (`server`) that keeps agents alive across detaches.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ratatui::crossterm;
use roster_detect::Detector;
use roster_proto::{read_frame, write_frame, Frame};
use roster_tui::SidebarSide;

mod app;
mod cli;
mod hook;
mod keys;
mod server;

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
    if args.print_hooks {
        print!("{}", hook::settings_snippet());
        return Ok(());
    }

    match &args.action {
        Some(cli::Action::Server(name)) => {
            let result = server::run(name);
            if let (Err(error), Some(log)) = (&result, std::env::var_os("ROSTER_SERVER_LOG")) {
                let _ = std::fs::write(log, format!("server {name}: {error}\n"));
            }
            return result;
        }
        Some(cli::Action::Proxy(name)) => return proxy(name),
        Some(cli::Action::Hook) => return hook::run(),
        Some(cli::Action::List) => return list_sessions(),
        Some(cli::Action::Kill(name)) => return kill_session(name),
        Some(cli::Action::Attach(target)) => return attach(target, &args),
        None => {}
    }

    if let Some(name) = &args.session {
        return run_session(name, &args, true);
    }

    let detector = load_detector(args.config.as_deref())?;
    // Bare `roster` opens a shell pane with the agent launcher over it —
    // agents are picked interactively, not supplied up front.
    let bare_start = args.commands.is_empty();
    let commands = if bare_start {
        vec![app::default_shell()]
    } else {
        args.commands.clone()
    };
    let app = app::App::new(detector, &commands, side_of(&args), bare_start)?;
    run_app(app)
}

fn side_of(args: &cli::Args) -> SidebarSide {
    match args.sidebar {
        Some(cli::Side::Right) => SidebarSide::Right,
        Some(cli::Side::Left) | None => SidebarSide::Left,
    }
}

/// Drive one App to completion inside the terminal, then print how a
/// session attachment ended, if it was one.
fn run_app(mut app: app::App) -> Result<(), String> {
    let mut terminal = ratatui::init();
    // Mouse-native: capture clicks, drags, and the wheel. Bracketed paste
    // delivers pastes as one event instead of a burst of keystrokes.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = app.run(&mut terminal);
    // Hand the pointer shape back to the terminal (OSC 22).
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::style::Print("\x1b]22;default\x07")
    );
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    if let Some(message) = app.exit_message() {
        println!("{message}");
    }
    result.map_err(|e| e.to_string())
}

/// Run inside the persistent session `name`, creating it when `create` and
/// it doesn't exist yet.
fn run_session(name: &str, args: &cli::Args, create: bool) -> Result<(), String> {
    if !server::valid_name(name) {
        return Err(format!(
            "invalid session name {name:?} (letters, digits, - and _)"
        ));
    }
    if !server::session_alive(name) {
        if !create {
            return Err(format!("no session named {name} — `roster ls` lists them"));
        }
        spawn_server(name)?;
    }
    let path = server::socket_path(name).ok_or("no home directory")?;
    let stream = UnixStream::connect(&path).map_err(|e| format!("connecting to {name}: {e}"))?;
    let reader = stream
        .try_clone()
        .map_err(|e| format!("cloning connection: {e}"))?;
    let detector = load_detector(args.config.as_deref())?;
    let app = app::App::new_remote(
        detector,
        side_of(args),
        name,
        Box::new(reader),
        Box::new(stream),
        &args.commands,
    )?;
    run_app(app)
}

/// Start the session server as a detached background process and wait for
/// its socket to answer.
fn spawn_server(name: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    // Vet the socket dir client-side too: the detached server's stderr goes
    // to /dev/null, so its refusal of a hostile dir would otherwise surface
    // only as "never came up".
    let dir = server::sessions_dir().ok_or("no home directory")?;
    server::ensure_private_dir(&dir)?;
    let exe = std::env::current_exe().map_err(|e| format!("finding roster binary: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("_server")
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // Its own process group: terminal-close signals aimed at the
        // client can't reach the server.
        .process_group(0);
    command
        .spawn()
        .map_err(|e| format!("starting session server: {e}"))?;
    for _ in 0..100 {
        if server::session_alive(name) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(format!("session server for {name} never came up"))
}

/// `roster attach <name>` — or `user@host:name` to attach over ssh.
fn attach(target: &str, args: &cli::Args) -> Result<(), String> {
    if let Some((host, name)) = target.split_once(':') {
        return attach_ssh(host, name, args);
    }
    run_session(target, args, false)
}

/// Attach to a session on another machine: run `roster _proxy <name>`
/// there over ssh and speak the protocol across its stdio.
fn attach_ssh(host: &str, name: &str, args: &cli::Args) -> Result<(), String> {
    if !server::valid_name(name) {
        return Err(format!("invalid session name {name:?}"));
    }
    let mut child = std::process::Command::new("ssh")
        .arg("-T")
        .arg(host)
        .arg("--")
        .arg("roster")
        .arg("_proxy")
        .arg(name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("running ssh: {e}"))?;
    let writer = child.stdin.take().expect("piped stdin");
    let reader = child.stdout.take().expect("piped stdout");
    let detector = load_detector(args.config.as_deref())?;
    let app = app::App::new_remote(
        detector,
        side_of(args),
        &format!("{host}:{name}"),
        Box::new(reader),
        Box::new(writer),
        &args.commands,
    )
    .map_err(|e| format!("{e} (is roster installed on {host}, and the session running?)"))?;
    let result = run_app(app);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// The remote half of ssh attach: bridge stdio to the local session
/// socket, byte for byte, both ways.
fn proxy(name: &str) -> Result<(), String> {
    if !server::valid_name(name) {
        return Err(format!("invalid session name {name:?}"));
    }
    if !server::session_alive(name) {
        return Err(format!("no session named {name} on this machine"));
    }
    let path = server::socket_path(name).ok_or("no home directory")?;
    let stream = UnixStream::connect(&path).map_err(|e| format!("connecting: {e}"))?;
    let mut socket_read = stream
        .try_clone()
        .map_err(|e| format!("cloning connection: {e}"))?;
    let mut socket_write = stream;

    // stdin → socket on a thread; socket → stdout here. Either side
    // ending tears the bridge down.
    let to_socket = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = socket_write.flush();
                }
            }
        }
        let _ = socket_write.shutdown(std::net::Shutdown::Write);
    });
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        match socket_read.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stdout.flush();
            }
        }
    }
    drop(stdout);
    let _ = to_socket.join();
    Ok(())
}

/// `roster ls` — list sessions, sweeping dead sockets.
fn list_sessions() -> Result<(), String> {
    let Some(dir) = server::sessions_dir() else {
        return Err("no home directory".to_string());
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => {
            println!("no sessions");
            return Ok(());
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".sock").map(str::to_string)
        })
        .collect();
    names.sort();
    let mut any = false;
    for name in names {
        if server::session_alive(&name) {
            println!("{name}");
            any = true;
        } else {
            // A socket with no server behind it is litter from a crash.
            if let Some(path) = server::socket_path(&name) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if !any {
        println!("no sessions");
    }
    Ok(())
}

/// `roster kill <name>` — end a session and every agent in it.
fn kill_session(name: &str) -> Result<(), String> {
    if !server::valid_name(name) {
        return Err(format!("invalid session name {name:?}"));
    }
    let path = server::socket_path(name).ok_or("no home directory")?;
    let Ok(mut stream) = UnixStream::connect(&path) else {
        let _ = std::fs::remove_file(&path);
        return Err(format!("no session named {name}"));
    };
    write_frame(&mut stream, &Frame::Kill).map_err(|e| format!("killing {name}: {e}"))?;
    // Wait for the server to actually go: it closes the socket on exit.
    while let Ok(Some(_)) = read_frame(&mut stream) {}
    println!("killed {name}");
    Ok(())
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
