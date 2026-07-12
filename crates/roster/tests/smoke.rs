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
    // The launcher modal is the mode indicator; no LAUNCH badge doubles it
    // in the corner. "new agent" above can match the pinned sidebar chip on
    // a pre-launcher frame, so wait for launch mode's own footer hint — it
    // shares the badge's row and paints after it, so once it shows, a
    // reintroduced badge would be visible too.
    assert!(
        drain_while(&mut screen, "type to filter", true, &rx),
        "launch-mode footer hint never rendered:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    let status_row = lines
        .iter()
        .find(|l| l.contains("type to filter"))
        .expect("hint row just drained into view");
    // Case-sensitive on purpose: the same row hints "enter: launch".
    assert!(
        !status_row.contains("LAUNCH"),
        "LAUNCH badge rendered with the launcher open:\n{status_row}"
    );
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");

    // The fake agent's blocked prompt must reach a pane and the sidebar.
    assert!(
        drain_while(&mut screen, "1 blocked", true, &rx),
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

    // 120x30 frame, chrome inset (2,1): sidebar 2..34, panes 34..118 in
    // rounded panels, status row 29 (1-based). Two shell panes split 42/42
    // at local x 0..42/42..84.
    let (cols, rows) = (120u16, 30u16);
    let mut pty =
        Pty::spawn(&format!("'{}' 'sleep 60' 'sleep 70'", bin()), cols, rows).expect("spawn");
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // The second command has focus at startup; the status line names it
    // (the footer sets hints a wide gap apart).
    assert!(
        drain_while(&mut screen, "focused ▸ sleep 70  ·  ctrl-b", true, &rx),
        "first frame:\n{}",
        screen.grid().lines().join("\n")
    );

    // Hovering the left pane's ✕ (motion is SGR button 35; the button
    // rides the title border, 1-based row 2) must switch the terminal
    // pointer to a hand via OSC 22.
    pty.write(b"\x1b[<35;74;2M").expect("hover ✕");
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
        drain_while(&mut screen, "focused ▸ sleep 60  ·  ctrl-b", true, &rx),
        "click did not focus the left pane:\n{}",
        screen.grid().lines().join("\n")
    );

    // Drag the divider between the halves (local col 41 → absolute 1-based
    // 76) to the left; the left panel's border must land near absolute
    // 0-based col 55.
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
    // y 27 → 1-based 28) opens the launcher; click the sole claude-code row
    // to launch it. Modal at 120x30: width 44 → x 38..82; height 5 → y 8..13
    // (0-based); the input sits at y 9, the claude-code row at y 10 → 1-based
    // 11.
    pty.write(&click(5, 28)).expect("click + new agent");
    assert!(
        drain_while(&mut screen, "new agent", true, &rx),
        "launcher never opened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(45, 11)).expect("click claude-code row");
    assert!(
        drain_while(&mut screen, "1 blocked", true, &rx),
        "clicked launch never went blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // The launched agent opened in its own window and has focus. The flat
    // sidebar lists agents only — the shell-only workspace has no rows —
    // so the status row's `⧉ 2/2` indicator (chrome right edge, 1-based
    // row 29) cycles back to the shells, and the agent's card (top of the
    // sidebar, 1-based rows 4-5) jumps to the agent again.
    pty.write(&click(116, 29))
        .expect("click status windows indicator");
    assert!(
        drain_while(&mut screen, "focused ▸ sleep 60  ·  ctrl-b", true, &rx),
        "status indicator click did not switch windows:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(&click(5, 4)).expect("click sidebar card");
    assert!(
        drain_while(&mut screen, "focused ▸ claude  ·  ctrl-b", true, &rx),
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
        drain_while(&mut screen, "focused ▸ sleep 70  ·  ctrl-b", true, &rx),
        "first frame:\n{}",
        screen.grid().lines().join("\n")
    );

    // Click the "solo" pill in the status row's layout switcher (status
    // row 1-based 29, 0-based cols 111..117 → 1-based click at 115, 29).
    pty.write(&click(115, 29)).expect("click solo");
    assert!(
        drain_while(
            &mut screen,
            "focused ▸ sleep 70  ·  click a card",
            true,
            &rx
        ),
        "solo never engaged:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert!(
        lines.iter().any(|l| l.contains("SOLO")),
        "no SOLO badge:\n{}",
        lines.join("\n")
    );
    // One panel only: its two side borders — no interior boundary in solo
    // (the sidebar separates by spacing now, not a rule).
    assert_eq!(
        lines[5].matches('│').count(),
        2,
        "screen:\n{}",
        lines.join("\n")
    );

    // Focus-next while solo shows the other pane, still solo.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"o").expect("focus next");
    assert!(
        drain_while(
            &mut screen,
            "focused ▸ sleep 60  ·  click a card",
            true,
            &rx
        ),
        "solo did not follow focus:\n{}",
        screen.grid().lines().join("\n")
    );

    // Clicking the "grid" pill (0-based cols 104..110) returns to the
    // tiles: two panels, four side borders.
    pty.write(&click(108, 29)).expect("click grid");
    assert!(
        drain_while(&mut screen, "focused ▸ sleep 60  ·  ctrl-b", true, &rx),
        "grid never returned:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert_eq!(
        lines[5].matches('│').count(),
        4,
        "screen:\n{}",
        lines.join("\n")
    );

    // Double-clicking a pane's title (the top border row, 1-based 2) also
    // goes solo.
    pty.write(&click(40, 2)).expect("first click");
    pty.write(&click(40, 2)).expect("second click");
    assert!(
        drain_while(
            &mut screen,
            "focused ▸ sleep 60  ·  click a card",
            true,
            &rx
        ),
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

    // The overlay card carries restart and close buttons. Sync on "close"
    // first — the rightmost button text, painted after everything else on
    // the buttons row — because the card title above can arrive a chunk
    // ahead of the buttons (a torn frame; flaked on Linux CI).
    assert!(
        drain_while(&mut screen, "close", true, &rx),
        "overlay buttons never rendered:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("restart") && l.contains("close")),
        "overlay buttons missing:\n{}",
        lines.join("\n")
    );

    // Clicking the title's ✕ closes the only (exited) pane and ends the
    // session. 100x24 frame, chrome inset (2,1): the pane region is 64
    // wide, the panel spans 34..98, its ✕ target at absolute 0-based cols
    // 94..97 on the top border row → 1-based (96, 2).
    pty.write(&click(96, 2)).expect("click ✕");
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

/// A bare `roster` at 120x30 with a fake `claude` on `PATH` and the
/// placeholder shell pinned to `/bin/sh` (host-independent), drained until
/// the welcome screen's run-a-command hint is up. Returns the fake-agent
/// dir (caller removes it), the pty, its pump channel, and the screen.
fn bare_start() -> (PathBuf, Pty, mpsc::Receiver<Vec<u8>>, Screen) {
    let dir = fake_agent_dir();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::env::set_var("PATH", &path);
    std::env::set_var("SHELL", "/bin/sh");

    let (cols, rows) = (120u16, 30u16);
    let pty = Pty::spawn(&format!("'{}'", bin()), cols, rows).expect("spawn roster");
    let rx = pump(&pty);
    let mut screen = Screen::new(cols, rows);
    assert!(
        drain_while(&mut screen, "run a command", true, &rx),
        "welcome screen never appeared:\n{}",
        screen.grid().lines().join("\n")
    );
    (dir, pty, rx, screen)
}

#[test]
fn bare_start_first_launch_replaces_the_placeholder_shell() {
    let (dir, mut pty, rx, mut screen) = bare_start();

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
        drain_while(&mut screen, "1 blocked", true, &rx),
        "launched agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );
    // "1 blocked" sits on the header row, which ratatui paints first —
    // on a slow PTY the wait above can return mid-frame, before the rows
    // below exist. The footer paints last, so its post-launch text is the
    // frame-complete barrier the geometry assertions need.
    assert!(
        drain_while(&mut screen, "focused ▸ claude", true, &rx),
        "post-launch footer never painted:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    // A single full-width pane: one panel, two side borders (the sidebar
    // separates by spacing, not a rule); a split would add two more.
    let rules = lines[5].matches('│').count();
    assert_eq!(
        rules,
        2,
        "expected one panel, screen:\n{}",
        lines.join("\n")
    );
    assert!(
        !lines[1].contains("○ sh"),
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
fn typing_into_the_backdrop_shell_does_not_save_it() {
    let (dir, mut pty, rx, mut screen) = bare_start();

    // Dismiss the launcher and let it flush as a lone Esc (a following byte
    // would coalesce into an Alt-chord) before typing into the backdrop
    // shell. Typing used to "claim" the shell into a durable pane; it must
    // not anymore, so the launch below still replaces it rather than leaving
    // it beside the agent.
    pty.write(b"\x1b").expect("close launcher");
    assert!(
        drain_while(&mut screen, "run a command", false, &rx),
        "launcher never closed:\n{}",
        screen.grid().lines().join("\n")
    );
    // No trailing newline: the marker echoes on the shell's input line
    // without running a command, so the shell stays alive as the backdrop.
    pty.write(b"roster-mark").expect("type into shell");
    assert!(
        drain_while(&mut screen, "roster-mark", true, &rx),
        "backdrop shell never echoed the typed marker:\n{}",
        screen.grid().lines().join("\n")
    );

    // Reopen the launcher and launch the agent.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"c").expect("open launcher");
    assert!(
        drain_while(&mut screen, "7Mb,od8", true, &rx),
        "launcher never reopened:\n{}",
        screen.grid().lines().join("\n")
    );
    pty.write(b"cla").expect("filter");
    pty.write(b"\r").expect("launch");
    assert!(
        drain_while(&mut screen, "1 blocked", true, &rx),
        "launched agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // Frame-complete barrier before geometry, as in the bare-start test:
    // the blocked wait matches the header row, painted before the rest.
    assert!(
        drain_while(&mut screen, "focused ▸ claude", true, &rx),
        "post-launch footer never painted:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    // One window only: a `⧉` workspace tag renders only with more than one
    // window, so its absence proves the typed-into shell did not survive as
    // its own workspace. A content row also holds one panel's two side
    // borders — a stray split would add two more.
    assert!(
        !lines.iter().any(|l| l.contains('⧉')),
        "a stray shell workspace survived (⧉ tag present):\n{}",
        lines.join("\n")
    );
    let rules = lines[5].matches('│').count();
    assert_eq!(
        rules,
        2,
        "expected one panel, screen:\n{}",
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
        drain_while(&mut screen, "1 blocked", true, &rx),
        "agent never showed blocked:\n{}",
        screen.grid().lines().join("\n")
    );

    // prefix-x on a live agent must ask, not kill.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"x").expect("close");
    assert!(
        drain_while(&mut screen, "no undo", true, &rx),
        "no close confirmation:\n{}",
        screen.grid().lines().join("\n")
    );
    // The confirm modal is the mode indicator; no CLOSE? badge doubles it
    // in the corner. Sync on the confirm-mode footer hint — it shares the
    // badge's row and paints after it, so once it shows, a reintroduced
    // badge would be visible too. (draw_hotkeys renders "y/enter: close"
    // with the colon dropped.)
    assert!(
        drain_while(&mut screen, "y/enter close", true, &rx),
        "confirm-mode footer hint never rendered:\n{}",
        screen.grid().lines().join("\n")
    );
    let lines = screen.grid().lines();
    let status_row = lines
        .iter()
        .find(|l| l.contains("y/enter close"))
        .expect("hint row just drained into view");
    assert!(
        !status_row.contains("CLOSE?"),
        "CLOSE? badge rendered with the confirm modal open:\n{status_row}"
    );

    // Esc cancels: the prompt clears and the agent pane survives.
    pty.write(b"\x1b").expect("cancel");
    assert!(
        drain_while(&mut screen, "no undo", false, &rx),
        "confirmation never cleared:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        screen.grid().lines()[1].contains("claude"),
        "agent pane gone after cancel:\n{}",
        screen.grid().lines().join("\n")
    );

    // Ask again and confirm with y: the last pane closes and roster exits.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"x").expect("close");
    assert!(
        drain_while(&mut screen, "no undo", true, &rx),
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
    // lines: the agent name on one, the reason on the next; the header
    // counts the blocked agent. The reason marker carries the sidebar's
    // truncation ellipsis and card indent, so a match proves the verbatim
    // prompt reached the card — the pane's own copy of the prompt is
    // full-width and can't satisfy it.
    let mut screen = Screen::new(cols, rows);
    let saw_blocked = drain_while(&mut screen, "claude-code", true, &rx)
        && drain_while(&mut screen, "1 blocked", true, &rx)
        && drain_while(&mut screen, "   Do you want to pro", true, &rx);
    assert!(
        saw_blocked,
        "sidebar never showed the blocked agent with its reason; screen was:\n{}",
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
fn unfocused_done_pane_stays_done_until_visited() {
    use std::os::unix::fs::PermissionsExt;

    // A fake claude that works briefly (a real working pattern, so activity
    // is on record), then clears to a settled result line over an idle
    // prompt. A custom agents.toml shrinks done.after_activity_secs to 2 so
    // the test proves the latch holds PAST the window without sleeping
    // through the shipped 8 seconds. The script is launched by absolute
    // path — the basename still identifies as claude-code and no PATH
    // mutation can race other tests.
    let dir = std::env::temp_dir().join(format!("roster-done-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fake agent dir");
    let script = dir.join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'esc to interrupt\\n'\nsleep 2\n\
         printf '\\033[2J\\033[H'\nprintf 'pumpernickel ready\\n'\nprintf '❯\\n'\nsleep 300\n",
    )
    .expect("write fake agent");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake agent");
    let config = dir.join("agents.toml");
    std::fs::write(
        &config,
        "[claude-code]\nmatch_command = [\"claude\"]\n\
         working = ['esc to interrupt']\nidle = ['^❯']\ndone.after_activity_secs = 2\n",
    )
    .expect("write config");

    // Two panes: the fake claude first, then the split that takes focus —
    // the pane under test finishes while it is NOT the focused one.
    let (cols, rows) = (120u16, 30u16);
    let mut pty = Pty::spawn(
        &format!(
            "'{}' --config '{}' '{}' 'sleep 300'",
            bin(),
            config.display(),
            script.display()
        ),
        cols,
        rows,
    )
    .expect("spawn roster");
    let rx = pump(&pty);
    let mut screen = Screen::new(cols, rows);

    // The unfocused pane settles and its card turns done: the ✓ glyph
    // appears (the card's and title's state signal — the reason alone
    // can't prove the state) with the result line as the reason.
    assert!(
        drain_while(&mut screen, "✓", true, &rx),
        "done glyph never appeared:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        drain_while(&mut screen, "    pumpe", true, &rx),
        "sidebar never showed done:\n{}",
        screen.grid().lines().join("\n")
    );

    // Sit well past the 2s window plus debounce: the timed decay must be
    // refused while the pane stays unfocused. The wait is a fixed 4s by
    // construction; done cannot flicker back once decayed (the screen is
    // static, nothing re-arms it), so the end-state assertion is exact.
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(4) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => screen.advance(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        screen.grid().lines().iter().any(|l| l.contains('✓')),
        "done state decayed while unfocused:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        screen
            .grid()
            .lines()
            .iter()
            .any(|l| l.contains("    pumpe")),
        "done reason decayed while unfocused:\n{}",
        screen.grid().lines().join("\n")
    );

    // Visit the pane (prefix-o cycles focus onto it): focus is the
    // acknowledgment, and the card decays to idle — the ✓ leaves with
    // the state, the reason with it.
    pty.write(&[0x02]).expect("prefix");
    pty.write(b"o").expect("focus next");
    assert!(
        drain_while(&mut screen, "✓", false, &rx),
        "done state never decayed after focusing the pane:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        drain_while(&mut screen, "    pumpe", false, &rx),
        "done reason never decayed after focusing the pane:\n{}",
        screen.grid().lines().join("\n")
    );
    // Three-space indent, not four: the visited card holds focus, so its
    // edge column carries the ▍ bar.
    assert!(
        drain_while(&mut screen, "   idle", true, &rx),
        "card never reached idle:\n{}",
        screen.grid().lines().join("\n")
    );

    pty.write(&[0x02]).expect("prefix");
    pty.write(b"q").expect("quit");
    let status = pty.wait().expect("wait");
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
        drain_while(&mut screen, "Bash: rm", true, &rx),
        "hook-reported ask never reached the sidebar:\n{}",
        screen.grid().lines().join("\n")
    );

    // The Stop hook releases the pane back to screen-based detection, and
    // the pinned reason leaves the sidebar.
    assert!(
        drain_while(&mut screen, "Bash: rm", false, &rx),
        "hook block never cleared:\n{}",
        screen.grid().lines().join("\n")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn statusline_telemetry_reaches_the_sidebar_card() {
    use std::os::unix::fs::PermissionsExt;

    // A fake claude that feeds the statusline bridge exactly like the real
    // one: Claude Code pipes the session JSON to the registered command,
    // which inherits ROSTER_PANE / ROSTER_HOOK_SOCK from the pane. The
    // screen never prints these numbers, so the badge below can only have
    // come through the socket.
    let dir = std::env::temp_dir().join(format!("roster-sl-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fake agent dir");
    let script = dir.join("claude");
    let payload = r#"{"model":{"display_name":"Opus"},"session_name":"Fix the auth flow","context_window":{"remaining_percentage":62.0},"cost":{"total_cost_usd":1.23}}"#;
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'thinking hard...\\n'\n\
             printf '%s' '{payload}' | '{roster}' _statusline\n\
             sleep 2\n",
            roster = bin(),
        ),
    )
    .expect("write fake agent");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake agent");

    let (cols, rows) = (120u16, 30u16);
    let pty = Pty::spawn(&format!("'{}' '{}'", bin(), script.display()), cols, rows)
        .expect("spawn roster");
    let rx = pump(&pty);

    let mut screen = Screen::new(cols, rows);

    // The full badge line on the card: model, context, cost.
    assert!(
        drain_while(&mut screen, "Opus · 62% context · $1.23", true, &rx),
        "statusline telemetry never reached the sidebar:\n{}",
        screen.grid().lines().join("\n")
    );

    // The payload's session name labels the card: the fake agent never
    // broadcasts a terminal title (the slash-command-first case), so the
    // name can only be the statusline fallback — not "claude-code".
    assert!(
        drain_while(&mut screen, "Fix the auth flow", true, &rx),
        "the session name never labeled the card:\n{}",
        screen.grid().lines().join("\n")
    );

    // The script exits (~2s): the pane lingers as an exited card, but its
    // badges must clear — frozen telemetry on a dead pane misleads.
    assert!(
        drain_while(&mut screen, "Opus · 62% context", false, &rx),
        "telemetry never cleared after the pane exited:\n{}",
        screen.grid().lines().join("\n")
    );
    assert!(
        drain_while(&mut screen, "exited (0)", true, &rx),
        "the exited card itself should linger:\n{}",
        screen.grid().lines().join("\n")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn statusline_forwarder_sends_the_payload_verbatim_and_prints_nothing() {
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    // The real binary, driven exactly as Claude Code drives its statusLine
    // command: session JSON on stdin, ROSTER_PANE / ROSTER_HOOK_SOCK in the
    // env (set on the child only — no process-global mutation).
    let dir = PathBuf::from(format!("/tmp/roster-sl{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("sock dir");
    let sock = dir.join("s.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind hook socket");
    listener.set_nonblocking(true).expect("nonblocking accept");

    let payload =
        r#"{"model":{"display_name":"Opus"},"context_window":{"remaining_percentage":41.5}}"#;
    let mut child = Command::new(bin())
        .arg("_statusline")
        .env("ROSTER_PANE", "7")
        .env("ROSTER_HOOK_SOCK", &sock)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn _statusline");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");

    let start = Instant::now();
    let mut conn = loop {
        match listener.accept() {
            Ok((conn, _)) => break conn,
            Err(_) if start.elapsed() < DEADLINE => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("_statusline never connected: {e}"),
        }
    };
    // macOS accepted sockets inherit the listener's nonblocking flag.
    conn.set_nonblocking(false).expect("blocking reads");
    // Best-effort: macOS refuses SO_RCVTIMEO (EINVAL) once the peer has
    // disconnected — and `_statusline` often writes and exits before this
    // line runs. In exactly that case the read can't block anyway: the
    // frame is buffered and EOF follows.
    let _ = conn.set_read_timeout(Some(DEADLINE));
    let frame = roster_proto::read_frame(&mut conn)
        .expect("read frame")
        .expect("one frame");
    assert_eq!(
        frame,
        roster_proto::Frame::Statusline {
            pane: 7,
            json: payload.into(),
        },
        "the payload must cross the socket verbatim"
    );

    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "must always exit 0");
    assert!(
        output.stdout.is_empty(),
        "stdout becomes the pane's visible statusline; it must stay empty"
    );

    // Outside a roster pane (no env) it is a silent, successful no-op.
    let output = Command::new(bin())
        .arg("_statusline")
        .output()
        .expect("run _statusline without env");
    assert!(output.status.success(), "no-op must still exit 0");
    assert!(output.stdout.is_empty(), "no-op must print nothing");

    let _ = std::fs::remove_dir_all(&dir);
}
