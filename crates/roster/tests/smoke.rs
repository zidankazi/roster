//! End-to-end smoke tests: the real binary, run headless inside a PTY.
//!
//! The full-pipeline test plants a fake `claude` on `PATH` that prints a
//! blocked prompt, runs roster in a pseudo-terminal, feeds roster's own
//! output bytes through `roster_term::Screen`, and asserts the sidebar
//! renders the blocked state with its reason — pty → term → detect → core →
//! tui, live.

use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use roster_pty::Pty;
use roster_term::Screen;

const DEADLINE: Duration = Duration::from_secs(15);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_roster")
}

#[test]
fn version_flag_prints_and_exits() {
    let output = Command::new(bin()).arg("--version").output().expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("roster "), "stdout: {stdout}");
}

#[test]
fn help_flag_prints_usage() {
    let output = Command::new(bin()).arg("--help").output().expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("USAGE"), "stdout: {stdout}");
    assert!(stdout.contains("prefix"), "stdout: {stdout}");
}

#[test]
fn print_config_dumps_builtin_agents() {
    let output = Command::new(bin())
        .arg("--print-config")
        .output()
        .expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[claude-code]"), "stdout: {stdout}");
    assert!(stdout.contains("match_command"), "stdout: {stdout}");
}

#[test]
fn unknown_flag_fails_with_message() {
    let output = Command::new(bin()).arg("--bogus").output().expect("run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown option"), "stderr: {stderr}");
}

#[test]
fn unreadable_config_fails_cleanly() {
    let output = Command::new(bin())
        .args(["--config", "/nonexistent/agents.toml"])
        .output()
        .expect("run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reading"), "stderr: {stderr}");
}

#[test]
fn launcher_spawns_an_agent_at_runtime() {
    // Start roster with a plain long-running shell command — no agents.
    // Open the launcher with ctrl-b c, type "cla" to filter to claude-code,
    // press enter, and the fake claude (on PATH) must appear as a pane with
    // a title bar and a sidebar card.
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);

    let (cols, rows) = (120u16, 30u16);
    let mut pty = Pty::spawn(&format!("'{}' 'sleep 60'", bin()), cols, rows).expect("spawn");
    let mut reader = pty.reader().expect("reader");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut screen = Screen::new(cols, rows);
    let drain_until = |screen: &mut Screen, needle: &str, rx: &mpsc::Receiver<Vec<u8>>| -> bool {
        let start = Instant::now();
        while start.elapsed() < DEADLINE {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => screen.advance(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
            if screen.grid().lines().iter().any(|l| l.contains(needle)) {
                return true;
            }
        }
        false
    };

    // Wait for the first frame (status line renders the hint text).
    assert!(
        drain_until(&mut screen, "ctrl-b", &rx),
        "first frame never rendered:\n{}",
        screen.grid().lines().join("\n")
    );

    // ctrl-b c → launcher; "cla" filters; enter launches.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"c").expect("open launcher");
    assert!(
        drain_until(&mut screen, "new agent", &rx),
        "launcher never opened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");

    // The fake agent's blocked prompt must reach a pane and the sidebar.
    assert!(
        drain_until(&mut screen, "blocked · Do y", &rx),
        "launched agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert!(
        lines.iter().any(|l| l.contains("● claude-code")),
        "no claude-code title/card:\n{}",
        lines.join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "roster exited with failure: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exited_pane_stays_until_closed() {
    let (cols, rows) = (100u16, 24u16);
    let mut pty =
        Pty::spawn(&format!("'{}' 'echo done'", bin()), cols, rows).expect("spawn roster");
    let mut reader = pty.reader().expect("reader");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut screen = Screen::new(cols, rows);
    let start = Instant::now();
    let mut saw_notice = false;
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => screen.advance(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if screen
            .grid()
            .lines()
            .iter()
            .any(|l| l.contains("exited (0)"))
        {
            saw_notice = true;
            break;
        }
    }
    assert!(
        saw_notice,
        "exited notice never appeared; screen was:\n{}",
        screen.grid().lines().join("\n")
    );

    // Closing the only (exited) pane ends the session.
    pty.write(&[0x02]).expect("write prefix");
    pty.write(b"x").expect("write x");
    let status = pty.wait().expect("wait");
    assert!(status.success, "roster exited with failure: {status:?}");
}

/// Create an executable fake agent named `claude` that shows a blocked
/// prompt, and return the directory holding it.
fn fake_agent_dir() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("roster-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fake agent dir");
    let script = dir.join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'Do you want to proceed?\\n'\nprintf '> 1. Yes\\n'\nsleep 30\n",
    )
    .expect("write fake agent");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake agent");
    dir
}

#[test]
fn full_pipeline_shows_blocked_agent_and_quits() {
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // Pty::spawn runs through `sh -c` and inherits this process's env.
    std::env::set_var("PATH", &path);

    let (cols, rows) = (120u16, 30u16);
    let mut pty = Pty::spawn(&format!("'{}' claude", bin()), cols, rows).expect("spawn roster");
    let mut reader = pty.reader().expect("reader");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    // Roster's own output is a terminal byte stream: parse it with our
    // emulator and watch the screen it draws.
    let mut screen = Screen::new(cols, rows);
    let start = Instant::now();
    let mut saw_blocked = false;
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => screen.advance(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // The sidebar card is two lines: the agent name on one, the state
        // and reason (truncated) on the next.
        let lines = screen.grid().lines();
        let has_name = lines.iter().any(|l| l.contains("claude-code"));
        let has_reason = lines.iter().any(|l| l.contains("blocked · Do y"));
        if has_name && has_reason {
            saw_blocked = true;
            break;
        }
    }
    assert!(
        saw_blocked,
        "sidebar never showed the blocked agent; screen was:\n{}",
        screen.grid().lines().join("\n")
    );

    // The pane itself must show the agent's actual output too.
    assert!(
        screen
            .grid()
            .lines()
            .iter()
            .any(|l| l.contains("Do you want to proceed?")),
        "pane content missing; screen was:\n{}",
        screen.grid().lines().join("\n")
    );

    // prefix-q quits: ctrl-b then q.
    pty.write(&[0x02]).expect("write prefix");
    pty.write(b"q").expect("write q");
    let started = Instant::now();
    let status = pty.wait().expect("wait");
    assert!(
        started.elapsed() < DEADLINE,
        "roster did not exit after prefix-q"
    );
    assert!(status.success, "roster exited with failure: {status:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
