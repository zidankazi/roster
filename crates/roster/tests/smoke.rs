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

/// One shared socket dir for every session test in this process: the env
/// var is process-global, so concurrent tests must agree on its value.
/// Short on purpose — unix socket paths cap out around 104 bytes on macOS.
fn smoke_sock_dir() -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/roster-t{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("sock dir");
    std::env::set_var("ROSTER_SOCK_DIR", &dir);
    dir
}

/// Pump the pty's output on a background thread, forwarding each chunk to
/// the returned channel until the pty closes.
fn pump(pty: &Pty) -> mpsc::Receiver<Vec<u8>> {
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
    rx
}

/// Advance `screen` with pty output until the needle's presence matches
/// `want` — wait for it to appear (`true`) or clear (`false`). False when
/// the deadline passes or the pty closes first.
fn drain_while(
    screen: &mut Screen,
    needle: &str,
    want: bool,
    rx: &mpsc::Receiver<Vec<u8>>,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        let present = screen.grid().lines().iter().any(|l| l.contains(needle));
        if present == want {
            return true;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => screen.advance(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
    false
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
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // Wait for the first frame (status line renders the hint text).
    assert!(
        drain_while(&mut screen, "ctrl-b", true, &rx),
        "first frame never rendered:\n{}",
        screen.grid().lines().join("\n")
    );

    // ctrl-b c → launcher; "cla" filters; enter launches.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"c").expect("open launcher");
    assert!(
        drain_while(&mut screen, "new agent", true, &rx),
        "launcher never opened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");

    // The fake agent's blocked prompt must reach a pane and the sidebar.
    assert!(
        drain_while(&mut screen, "blocked · Do y", true, &rx),
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
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // The second command has focus at startup; the status line names it.
    assert!(
        drain_while(&mut screen, "sleep 70   click", true, &rx),
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
        drain_while(&mut screen, "sleep 60   click", true, &rx),
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
        drain_while(&mut screen, "new agent", true, &rx),
        "launcher never opened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(45, 11)).expect("click claude-code row");
    assert!(
        drain_while(&mut screen, "blocked · Do y", true, &rx),
        "clicked launch never went blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // The launched agent opened in its own window and has focus. The
    // sidebar now lists both workspaces — the shell-only one included —
    // so clicking the "workspace 1" header (1-based row 3) jumps back to
    // the shells, and the agent's card (rows 7-8, under the "workspace 2"
    // header) jumps to the agent again.
    pty.write(&click(5, 3)).expect("click workspace 1 header");
    assert!(
        drain_while(&mut screen, "sleep 60   click", true, &rx),
        "workspace header click did not switch windows:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(5, 7)).expect("click sidebar card");
    assert!(
        drain_while(&mut screen, "claude   click", true, &rx),
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
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    assert!(
        drain_while(&mut screen, "sleep 70   click", true, &rx),
        "first frame:\n{}",
        screen.grid().lines().join("\n")
    );

    // Click "solo" in the sidebar's layout switcher (row above the + new
    // agent button: 0-based y 27 → 1-based 28; word at cols 8..12).
    pty.write(&click(10, 28)).expect("click solo");
    assert!(
        drain_while(&mut screen, "sleep 70   click agents", true, &rx),
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
        drain_while(&mut screen, "sleep 60   click agents", true, &rx),
        "solo did not follow focus:\n{}",
        screen.grid().lines().join("\n")
    );

    // Clicking "grid" in the switcher returns to the tiles: the interior
    // separator is back.
    pty.write(&click(3, 28)).expect("click grid");
    assert!(
        drain_while(&mut screen, "sleep 60   click a pane", true, &rx),
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
        drain_while(&mut screen, "sleep 60   click agents", true, &rx),
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
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);
    assert!(
        drain_while(&mut screen, "echo · exit 0", true, &rx),
        "exited overlay never appeared; screen was:\n{}",
        screen.grid().lines().join("\n")
    );

    // The overlay card carries restart and close buttons.
    let lines = screen.grid().lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("restart") && l.contains("close")),
        "overlay buttons missing:\n{}",
        lines.join("\n")
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
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // Bare `roster` opens the welcome screen: wordmark + picker + the
    // run-a-command hint.
    assert!(
        drain_while(&mut screen, "run a command", true, &rx),
        "welcome screen never appeared:\n{}",
        screen.grid().lines().join("\n")
    );
    // The wordmark sweeps in over ~1s; wait for its leading glyphs.
    assert!(
        drain_while(&mut screen, "7Mb,od8", true, &rx),
        "no wordmark:\n{}",
        screen.grid().lines().join("\n")
    );

    // Launch the first agent: it must take the placeholder's place, not
    // split it — no leftover empty shell pane.
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");
    assert!(
        drain_while(&mut screen, "blocked · Do y", true, &rx),
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
fn closing_a_live_agent_asks_first() {
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);

    let (cols, rows) = (120u16, 30u16);
    let mut pty = Pty::spawn(&format!("'{}' claude", bin()), cols, rows).expect("spawn roster");
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    assert!(
        drain_while(&mut screen, "blocked · Do y", true, &rx),
        "agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // prefix-x on a live agent must ask, not kill.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"x").expect("close");
    assert!(
        drain_while(&mut screen, "still running", true, &rx),
        "no close confirmation:\n{}",
        screen.grid().lines().join("\n")
    );

    // Esc cancels: the prompt clears and the agent pane survives.
    pty.write(b"\x1b").expect("cancel");
    assert!(
        drain_while(&mut screen, "still running", false, &rx),
        "confirmation never cleared:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        screen.grid().lines()[0].contains("claude"),
        "agent pane gone after cancel:\n{}",
        screen.grid().lines().join("\n")
    );

    // Ask again and confirm with y: the last pane closes and roster exits.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"x").expect("close");
    assert!(
        drain_while(&mut screen, "still running", true, &rx),
        "no second confirmation:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(b"y").expect("confirm");
    let status = pty.wait().expect("wait");
    assert!(status.success, "exit: {status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wheel_scrolls_history_and_typing_snaps_back() {
    let (cols, rows) = (100u16, 24u16);
    // 200 numbered lines then a live shell read: plenty of history, primary
    // screen (no alternate-screen TUI), process stays alive.
    let mut pty =
        Pty::spawn(&format!("'{}' 'seq 1 200; cat'", bin()), cols, rows).expect("spawn roster");
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // The tail of the output is on screen; line 1 has scrolled away. Gate
    // on a line number that appears only in the output — the status line
    // echoes the command, which contains "200".
    assert!(
        drain_while(&mut screen, "197", true, &rx),
        "tail never appeared:\n{}",
        screen.grid().lines().join("\n")
    );

    // Wheel up over the pane content (SGR button 64) far enough to reach
    // early history; the scrolled chip appears.
    for _ in 0..30 {
        pty.write(b"\x1b[<64;60;10M").expect("wheel up");
    }
    assert!(
        drain_while(&mut screen, "↑", true, &rx),
        "scroll chip never appeared:\n{}",
        screen.grid().lines().join("\n")
    );

    // Typing snaps back to live output: the chip clears.
    pty.write(b"x").expect("type");
    assert!(
        drain_while(&mut screen, "↑", false, &rx),
        "chip never cleared after typing:\n{}",
        screen.grid().lines().join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
    assert!(status.success, "exit: {status:?}");
}

#[test]
fn ssh_proxy_bridges_the_protocol_over_stdio() {
    use roster_proto::{read_frame, write_frame, Frame};

    // The remote half of ssh attach is `roster _proxy <name>`: it bridges
    // stdio to the session socket. Driving it through pipes exercises the
    // exact transport ssh would carry.
    let state = smoke_sock_dir();
    let name = format!("proxy{}", std::process::id());

    let mut server = Command::new(bin())
        .args(["_server", &name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn server");
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        let ls = Command::new(bin()).arg("ls").output().expect("ls");
        if String::from_utf8_lossy(&ls.stdout).contains(&name) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut proxy = Command::new(bin())
        .args(["_proxy", &name])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn proxy");
    let mut to_proxy = proxy.stdin.take().expect("stdin");
    let mut from_proxy = proxy.stdout.take().expect("stdout");

    // Attach → Hello (empty session).
    write_frame(&mut to_proxy, &Frame::Attach).expect("attach");
    match read_frame(&mut from_proxy).expect("read hello") {
        Some(Frame::Hello { panes, .. }) => assert!(panes.is_empty(), "fresh session"),
        other => panic!("expected Hello, got {other:?}"),
    }

    // Spawn a command and watch its output come back as frames.
    write_frame(
        &mut to_proxy,
        &Frame::Spawn {
            command: "echo proxied-hello; sleep 30".into(),
        },
    )
    .expect("spawn");
    let mut opened = None;
    let mut saw_output = false;
    let start = Instant::now();
    while start.elapsed() < DEADLINE && !saw_output {
        match read_frame(&mut from_proxy).expect("read frame") {
            Some(Frame::PaneOpened { pane, command }) => {
                assert!(command.contains("proxied-hello"));
                opened = Some(pane);
            }
            Some(Frame::Output { pane, bytes }) => {
                assert_eq!(Some(pane), opened, "output for the opened pane");
                if String::from_utf8_lossy(&bytes).contains("proxied-hello") {
                    saw_output = true;
                }
            }
            Some(_) => {}
            None => panic!("proxy closed early"),
        }
    }
    assert!(saw_output, "spawned output never arrived through the proxy");

    // Kill the session: we get a Shutdown, the bridge drains, everything
    // exits.
    write_frame(&mut to_proxy, &Frame::Kill).expect("kill");
    let start = Instant::now();
    let mut shut = false;
    while start.elapsed() < DEADLINE {
        match read_frame(&mut from_proxy) {
            Ok(Some(Frame::Shutdown { .. })) => {
                shut = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert!(shut, "no shutdown after kill");
    drop(to_proxy);
    let _ = proxy.wait();
    let _ = server.wait();
    let _ = state;
}

#[test]
fn read_paths_refuse_a_symlinked_socket_dir() {
    // A symlink where the session sockets should be is the world-writable-
    // /tmp swap attack: a local user plants /tmp/roster-<uid> pointing at a
    // dir they own. `ls`, `attach`, and `kill` must vet the dir and refuse
    // it — not follow the link to probe, connect to, or unlink sockets at
    // the attacker's chosen target. Each command reaches the sockets by a
    // different route (read_dir, attach-connect, kill-connect); all must
    // route through the same vet first.
    //
    // `.env` scopes ROSTER_SOCK_DIR to each child, so this never disturbs
    // the process-global value the other session tests share.
    let pid = std::process::id();
    let target = std::env::temp_dir().join(format!("roster-evil-target-{pid}"));
    std::fs::create_dir_all(&target).expect("attacker target dir");
    let link = std::env::temp_dir().join(format!("roster-evil-link-{pid}"));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&target, &link).expect("plant symlink");

    for args in [
        &["ls"][..],
        &["attach", "victim"][..],
        &["kill", "victim"][..],
    ] {
        let output = Command::new(bin())
            .args(args)
            .env("ROSTER_SOCK_DIR", &link)
            .output()
            .expect("run roster");
        assert!(
            !output.status.success(),
            "`roster {}` followed a symlinked socket dir",
            args.join(" ")
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not a directory"),
            "`roster {}` did not refuse via the dir vet; stderr: {stderr}",
            args.join(" ")
        );
    }

    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn persistent_session_survives_detach_and_reattach() {
    // Sessions live under ROSTER_SOCK_DIR; keep the test's sockets in a
    // scratch dir of their own (short: unix socket paths cap out ~104
    // bytes on macOS).
    let state = smoke_sock_dir();
    let name = format!("smoke{}", std::process::id());

    let (cols, rows) = (100u16, 24u16);

    // First client: create the session with a long-lived pane and leave a
    // marker in its output.
    let mut pty = Pty::spawn(
        &format!("'{}' -s {name} 'seq 1 50; cat'", bin()),
        cols,
        rows,
    )
    .expect("spawn roster -s");
    let rx = pump(&pty);
    let mut screen = Screen::new(cols, rows);
    assert!(
        drain_while(&mut screen, "47", true, &rx),
        "session pane never showed output:\n{}",
        screen.grid().lines().join("\n")
    );
    if let Err(error) = pty.write(b"marker123\r") {
        // The client died — drain its last words for the failure message.
        while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(300)) {
            screen.advance(&chunk);
        }
        panic!(
            "typing marker failed ({error}); final screen:\n{}",
            screen.grid().lines().join("\n")
        );
    }
    assert!(
        drain_while(&mut screen, "marker123", true, &rx),
        "marker never echoed:\n{}",
        screen.grid().lines().join("\n")
    );

    // Detach: the client exits and says how to come back; the server and
    // its pane keep running.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"d").expect("detach");
    assert!(
        drain_while(&mut screen, "detached — reattach with", true, &rx),
        "no detach message:\n{}",
        screen.grid().lines().join("\n")
    );
    let status = pty.wait().expect("wait detach");
    assert!(status.success, "detach exit: {status:?}");

    // The session shows up in `roster ls`.
    let ls = Command::new(bin()).arg("ls").output().expect("roster ls");
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains(&name),
        "ls output: {:?}",
        String::from_utf8_lossy(&ls.stdout)
    );

    // Reattach: the pane is still there, screen rebuilt from replay —
    // marker included.
    let mut pty =
        Pty::spawn(&format!("'{}' attach {name}", bin()), cols, rows).expect("spawn roster attach");
    let rx = pump(&pty);
    let mut screen = Screen::new(cols, rows);
    assert!(
        drain_while(&mut screen, "marker123", true, &rx),
        "replay never restored the marker:\n{}",
        screen.grid().lines().join("\n")
    );

    // Closing the last pane ends the session: client exits, server goes,
    // ls forgets it.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"x").expect("close pane");
    let status = pty.wait().expect("wait close");
    assert!(status.success, "close exit: {status:?}");
    let start = Instant::now();
    let mut gone = false;
    while start.elapsed() < DEADLINE {
        let ls = Command::new(bin()).arg("ls").output().expect("roster ls");
        if !String::from_utf8_lossy(&ls.stdout).contains(&name) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(gone, "session lingered after its last pane closed");
    let _ = state;
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
    let rx = pump(&pty);

    // Roster's own output is a terminal byte stream: parse it with our
    // emulator and watch the screen it draws. The sidebar card is two
    // lines: the agent name on one, the state and reason on the next.
    let mut screen = Screen::new(cols, rows);
    let saw_blocked = drain_while(&mut screen, "claude-code", true, &rx)
        && drain_while(&mut screen, "blocked · Do y", true, &rx);
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

#[test]
fn hook_bridge_pins_blocked_and_clears_end_to_end() {
    use std::os::unix::fs::PermissionsExt;

    // A fake claude that drives the hook bridge exactly like the real one:
    // it inherits ROSTER_PANE / ROSTER_HOOK_SOCK from its pane's env and
    // reports a permission ask via `roster _hook`, then answers it two
    // seconds later. Its screen never shows a blocked pattern, so the
    // sidebar reason below can only have come through the hook socket.
    let dir = std::env::temp_dir().join(format!("roster-hook-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fake agent dir");
    let script = dir.join("claude");
    let ask = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#;
    let stop = r#"{"hook_event_name":"Stop"}"#;
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'thinking hard...\\n'\n\
             printf '%s' '{ask}' | '{roster}' _hook\n\
             sleep 2\n\
             printf '%s' '{stop}' | '{roster}' _hook\n\
             sleep 30\n",
            roster = bin(),
        ),
    )
    .expect("write fake agent");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake agent");

    // The pane command is the script's absolute path: the basename still
    // identifies as claude-code, and no PATH mutation can race other tests.
    let (cols, rows) = (120u16, 30u16);
    let pty = Pty::spawn(&format!("'{}' '{}'", bin(), script.display()), cols, rows)
        .expect("spawn roster");
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // The hook-delivered ask, verbatim, in the sidebar — while the pane
    // itself only ever printed "thinking hard...".
    assert!(
        drain_while(&mut screen, "blocked · Bash: rm", true, &rx),
        "hook-reported ask never reached the sidebar:\n{}",
        screen.grid().lines().join("\n")
    );

    // The Stop hook releases the pane back to screen-based detection, and
    // the pinned reason leaves the sidebar.
    assert!(
        drain_while(&mut screen, "blocked · Bash: rm", false, &rx),
        "hook block never cleared:\n{}",
        screen.grid().lines().join("\n")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
