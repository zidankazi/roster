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
        lines.iter().any(|l| l.contains("◉ claude-code")),
        "no claude-code title/card:\n{}",
        lines.join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "roster exited with failure: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// SGR mouse press+release at 1-based (col, row).
fn click(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").into_bytes()
}

#[test]
fn mouse_clicks_focus_launch_and_jump() {
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);

    // 120x30 frame: sidebar 0..32 (rule at col 31), panes 32..120, status
    // row 30 (1-based). Two shell panes split 44/44 at local x 0..44/44..88.
    let (cols, rows) = (120u16, 30u16);
    let mut pty =
        Pty::spawn(&format!("'{}' 'sleep 60' 'sleep 70'", bin()), cols, rows).expect("spawn");
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

    // The second command has focus at startup; the status line names it.
    assert!(
        drain_until(&mut screen, "sleep 70   click", &rx),
        "first frame:\n{}",
        screen.grid().lines().join("\n")
    );

    // Hovering the left pane's ✕ (motion is SGR button 35) must switch the
    // terminal pointer to a hand via OSC 22.
    pty.write(b"\x1b[<35;74;1M").expect("hover ✕");
    let start = Instant::now();
    let mut raw: Vec<u8> = Vec::new();
    let mut saw_hand = false;
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                raw.extend_from_slice(&chunk);
                screen.advance(&chunk);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if raw.windows(13).any(|w| w == b"\x1b]22;pointer\x07") {
            saw_hand = true;
            break;
        }
    }
    assert!(saw_hand, "hover never emitted an OSC 22 hand pointer");

    // Click inside the first pane's content (absolute col ~40, row 10) —
    // focus follows the mouse click.
    pty.write(&click(40, 10)).expect("click left pane");
    assert!(
        drain_until(&mut screen, "sleep 60   click", &rx),
        "click did not focus the left pane:\n{}",
        screen.grid().lines().join("\n")
    );

    // Drag the divider between the halves (local col 43 → absolute 1-based
    // 76) to the left; the separator must land near local col 23 (absolute
    // 0-based 55).
    pty.write(b"\x1b[<0;76;10M").expect("grab divider");
    pty.write(b"\x1b[<32;66;10M").expect("drag");
    pty.write(b"\x1b[<32;56;10M").expect("drag");
    pty.write(b"\x1b[<0;56;10m").expect("release");
    let start = Instant::now();
    let mut moved = false;
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => screen.advance(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if screen
            .grid()
            .lines()
            .get(5)
            .is_some_and(|l| l.chars().nth(55) == Some('│'))
        {
            moved = true;
            break;
        }
    }
    assert!(
        moved,
        "divider never moved to column 55; row 5 was:\n{:?}",
        screen.grid().lines().get(5)
    );

    // The sidebar's pinned + new agent button (bottom sidebar row, 0-based
    // y 28 → 1-based 29) opens the launcher; click the claude-code row to
    // launch it. Modal at 120x30: width 44 → x 38..82; height 8 → y 7..15
    // (0-based); items start at y 9, claude-code is the second row → y 10
    // → 1-based 11.
    pty.write(&click(5, 29)).expect("click + new agent");
    assert!(
        drain_until(&mut screen, "new agent", &rx),
        "launcher never opened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(45, 11)).expect("click claude-code row");
    assert!(
        drain_until(&mut screen, "blocked · Do y", &rx),
        "clicked launch never went blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // The launched agent has focus; click its sidebar card (rows 3-4,
    // 1-based) after clicking back into a shell pane first.
    pty.write(&click(40, 10)).expect("refocus shell");
    assert!(
        drain_until(&mut screen, "sleep 60   click", &rx),
        "refocus failed:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(5, 3)).expect("click sidebar card");
    assert!(
        drain_until(&mut screen, "claude   click", &rx),
        "sidebar click did not jump to the agent:\n{}",
        screen.grid().lines().join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn solo_view_toggles_by_click_and_switches_with_focus() {
    let (cols, rows) = (120u16, 30u16);
    let mut pty =
        Pty::spawn(&format!("'{}' 'sleep 60' 'sleep 70'", bin()), cols, rows).expect("spawn");
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

    assert!(
        drain_until(&mut screen, "sleep 70   click", &rx),
        "first frame:\n{}",
        screen.grid().lines().join("\n")
    );

    // Click "solo" in the sidebar's layout switcher (row above the + new
    // agent button: 0-based y 27 → 1-based 28; word at cols 8..12).
    pty.write(&click(10, 28)).expect("click solo");
    assert!(
        drain_until(&mut screen, "sleep 70   click agents", &rx),
        "solo never engaged:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert!(
        lines.iter().any(|l| l.contains("SOLO")),
        "no SOLO badge:\n{}",
        lines.join("\n")
    );
    // One rule only — the sidebar's; no interior separator in solo.
    assert_eq!(
        lines[5].matches('│').count(),
        1,
        "screen:\n{}",
        lines.join("\n")
    );

    // Focus-next while solo shows the other pane, still solo.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"o").expect("focus next");
    assert!(
        drain_until(&mut screen, "sleep 60   click agents", &rx),
        "solo did not follow focus:\n{}",
        screen.grid().lines().join("\n")
    );

    // Clicking "grid" in the switcher returns to the tiles: the interior
    // separator is back.
    pty.write(&click(3, 28)).expect("click grid");
    assert!(
        drain_until(&mut screen, "sleep 60   click a pane", &rx),
        "grid never returned:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert_eq!(
        lines[5].matches('│').count(),
        2,
        "screen:\n{}",
        lines.join("\n")
    );

    // Double-clicking a pane's title also goes solo.
    pty.write(&click(40, 1)).expect("first click");
    pty.write(&click(40, 1)).expect("second click");
    assert!(
        drain_until(&mut screen, "sleep 60   click agents", &rx),
        "double-click did not go solo:\n{}",
        screen.grid().lines().join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "exit: {status:?}");
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

    // Clicking the title's ✕ closes the only (exited) pane and ends the
    // session. 100x24 frame: single pane content width 68 → ✕ target at
    // absolute cols 97..100, title row 0 → 1-based (98, 1).
    pty.write(&click(98, 1)).expect("click ✕");
    let status = pty.wait().expect("wait");
    assert!(status.success, "roster exited with failure: {status:?}");
}

/// Create an executable fake agent named `claude` that shows a blocked
/// prompt, and return the directory holding it. Each call gets its own
/// directory: tests run concurrently in one process, and on Linux exec'ing
/// a script another test is rewriting fails with ETXTBSY.
fn fake_agent_dir() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "roster-smoke-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
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
fn bare_start_first_launch_replaces_the_placeholder_shell() {
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);
    // Pin the placeholder shell so the test is host-independent.
    std::env::set_var("SHELL", "/bin/sh");

    let (cols, rows) = (120u16, 30u16);
    let mut pty = Pty::spawn(&format!("'{}'", bin()), cols, rows).expect("spawn roster");
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

    // Bare `roster` greets with the launcher over a placeholder shell.
    assert!(
        drain_until(&mut screen, "new agent", &rx),
        "launcher never greeted:\n{}",
        screen.grid().lines().join("\n")
    );

    // Launch the first agent: it must take the placeholder's place, not
    // split it — no leftover empty shell pane.
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");
    assert!(
        drain_until(&mut screen, "blocked · Do y", &rx),
        "launched agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    // A single full-width pane: a content row holds only the sidebar rule;
    // a split would add an interior separator.
    let rules = lines[5].matches('│').count();
    assert_eq!(rules, 1, "expected one rule, screen:\n{}", lines.join("\n"));
    assert!(
        !lines[0].contains("○ sh"),
        "placeholder shell still titled:\n{}",
        lines.join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
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
