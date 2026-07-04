//! Capture a live agent's screen as fixture text.
//!
//! Runs a command in a PTY, feeds its output through the emulator, and
//! prints the visible grid at each sample point. Use it to build fixtures
//! for `roster-detect` from real agent sessions:
//!
//! ```sh
//! cargo run -p roster --example capture -- "claude" 100 30 3,6,10
//! ```
//!
//! Arguments: command, cols, rows, comma-separated sample times in seconds,
//! then optional `sec:text` sends (with `\e` for escape and `\r` for
//! enter) to drive dialogs. Each sample prints the grid between
//! `--- t=Ns ---` markers; the child is killed after the last sample.

use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use roster_pty::Pty;
use roster_term::Screen;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: capture <command> <cols> <rows> <secs,secs,...> [sec:text ...]");
        std::process::exit(2);
    }
    let command = &args[0];
    let cols: u16 = args[1].parse().expect("cols");
    let rows: u16 = args[2].parse().expect("rows");
    let mut samples: Vec<u64> = args[3]
        .split(',')
        .map(|s| s.parse().expect("sample seconds"))
        .collect();
    samples.sort_unstable();
    let mut sends: Vec<(u64, Vec<u8>)> = args[4..]
        .iter()
        .map(|spec| {
            let (sec, text) = spec.split_once(':').expect("send spec is sec:text");
            let bytes = text
                .replace("\\e", "\x1b")
                .replace("\\r", "\r")
                .into_bytes();
            (sec.parse().expect("send seconds"), bytes)
        })
        .collect();
    sends.sort_by_key(|(sec, _)| *sec);

    let mut pty = Pty::spawn(command, cols, rows).expect("spawn");
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
    let mut sends = sends.into_iter().peekable();
    for &sample in &samples {
        let deadline = start + Duration::from_secs(sample);
        while Instant::now() < deadline {
            while sends
                .peek()
                .is_some_and(|(sec, _)| start + Duration::from_secs(*sec) <= Instant::now())
            {
                let (_, bytes) = sends.next().expect("peeked");
                pty.write(&bytes).expect("send input");
            }
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                screen.advance(&chunk);
            }
        }
        println!("--- t={sample}s ---");
        for line in screen.grid().lines() {
            println!("{line}");
        }
        println!("--- end t={sample}s ---");
    }
}
