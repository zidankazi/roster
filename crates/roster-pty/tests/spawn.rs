//! Integration tests against real PTYs and real child processes.
//!
//! Reads go through a pump thread with a deadline so a regression hangs a
//! channel receive, not the whole test run.

use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use roster_pty::Pty;

const DEADLINE: Duration = Duration::from_secs(10);

/// Pump the pty's output on a thread; return a receiver of chunks.
fn pump(pty: &Pty) -> mpsc::Receiver<Vec<u8>> {
    let mut reader = pty.reader().expect("clone reader");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// Collect output until `needle` appears or the deadline passes.
fn read_until(rx: &mpsc::Receiver<Vec<u8>>, needle: &str) -> String {
    let start = Instant::now();
    let mut collected = Vec::new();
    while start.elapsed() < DEADLINE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                collected.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&collected).contains(needle) {
                    return String::from_utf8_lossy(&collected).into_owned();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!(
        "`{needle}` not seen within {DEADLINE:?}; got: {:?}",
        String::from_utf8_lossy(&collected)
    );
}

#[test]
fn spawned_command_output_is_readable() {
    let mut pty = Pty::spawn("echo roster-hello", 80, 24).expect("spawn");
    let rx = pump(&pty);
    read_until(&rx, "roster-hello");
    let status = pty.wait().expect("wait");
    assert!(status.success);
}

#[test]
fn shell_quoting_is_respected() {
    let mut pty = Pty::spawn("printf '%s' 'a  b'", 80, 24).expect("spawn");
    let rx = pump(&pty);
    read_until(&rx, "a  b");
    assert!(pty.wait().expect("wait").success);
}

#[test]
fn exit_codes_come_back() {
    let mut pty = Pty::spawn("exit 3", 80, 24).expect("spawn");
    let status = pty.wait().expect("wait");
    assert_eq!(status.code, 3);
    assert!(!status.success);
}

#[test]
fn child_sees_the_requested_size() {
    let mut pty = Pty::spawn("stty size", 100, 40).expect("spawn");
    let rx = pump(&pty);
    read_until(&rx, "40 100");
    assert!(pty.wait().expect("wait").success);
}

#[test]
fn child_runs_in_the_current_directory() {
    let mut pty = Pty::spawn("pwd", 80, 24).expect("spawn");
    let rx = pump(&pty);
    let expected = std::env::current_dir().expect("cwd");
    read_until(&rx, &expected.to_string_lossy());
    assert!(pty.wait().expect("wait").success);
}

#[test]
fn term_is_advertised() {
    let mut pty = Pty::spawn("echo TERM=$TERM", 80, 24).expect("spawn");
    let rx = pump(&pty);
    read_until(&rx, "TERM=xterm-256color");
    assert!(pty.wait().expect("wait").success);
}

#[test]
fn written_input_reaches_the_child() {
    let mut pty = Pty::spawn("cat", 80, 24).expect("spawn");
    let rx = pump(&pty);
    pty.write(b"ping\r").expect("write");
    read_until(&rx, "ping");
    pty.kill().expect("kill");
    let status = pty.wait().expect("wait");
    assert!(!status.success);
}

#[test]
fn kill_stops_a_long_running_child() {
    let mut pty = Pty::spawn("sleep 30", 80, 24).expect("spawn");
    assert!(pty.process_id().is_some());
    assert_eq!(pty.try_wait().expect("try_wait"), None);
    let started = Instant::now();
    pty.kill().expect("kill");
    let status = pty.wait().expect("wait");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!status.success);
}

#[test]
fn drop_kills_a_sighup_immune_child() {
    // Agents like Claude Code trap SIGHUP; drop must escalate to SIGKILL
    // and take the whole process group with it.
    let pty = Pty::spawn("trap '' HUP; sleep 30", 80, 24).expect("spawn");
    let pid = pty.process_id().expect("pid").to_string();
    let started = Instant::now();
    drop(pty);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "drop took {:?}",
        started.elapsed()
    );

    // The process group must actually be gone (kill -0 fails once the
    // shell and its sleep child are dead and reaped).
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .status()
            .expect("run kill -0")
            .success();
        if !alive {
            break;
        }
        assert!(Instant::now() < deadline, "child {pid} still alive");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn resize_reaches_a_live_child() {
    // stty size reads the pty's current dimensions at exec time; resize
    // first, then run it via a second command in the same pty session.
    let mut pty = Pty::spawn("sleep 0.3; stty size", 90, 30).expect("spawn");
    pty.resize(120, 50).expect("resize");
    let rx = pump(&pty);
    read_until(&rx, "50 120");
    assert!(pty.wait().expect("wait").success);
}
