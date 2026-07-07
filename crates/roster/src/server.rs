//! The session server: a background process that owns the PTYs so agents
//! survive the UI. Clients attach over a unix socket, speak
//! `roster-proto`, and can detach and come back — the panes keep running.
//!
//! One client at a time: a new attach takes over and the old client is told
//! to shut down (the tmux model). The server keeps a replay buffer per pane
//! so a reattaching client can rebuild each screen, and stores the client's
//! layout blob verbatim — layout is the client's business.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use roster_proto::{read_frame, write_frame, Frame, HelloPane};
use roster_pty::Pty;

/// Cap on each pane's replay buffer: enough to repaint any screen and some
/// scrollback without hoarding gigabytes of agent chatter.
const REPLAY_CAP: usize = 256 * 1024;

/// Panes spawn at this size until the client's first resize.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

/// Where session sockets live: `$ROSTER_SOCK_DIR`, or `/tmp/roster-<uid>`
/// (the tmux convention). Unix socket paths must stay under ~104 bytes on
/// macOS, which rules out deep per-user state directories.
pub fn sessions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ROSTER_SOCK_DIR") {
        return Some(PathBuf::from(dir));
    }
    let uid = unsafe { libc::getuid() };
    Some(PathBuf::from(format!("/tmp/roster-{uid}")))
}

/// The socket path for a named session.
pub fn socket_path(name: &str) -> Option<PathBuf> {
    Some(sessions_dir()?.join(format!("{name}.sock")))
}

/// Session names stay path- and shell-safe.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Probe a session socket: `true` when a server answers.
pub fn session_alive(name: &str) -> bool {
    let Some(path) = socket_path(name) else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return false;
    };
    if write_frame(&mut stream, &Frame::Ping).is_err() {
        return false;
    }
    matches!(read_frame(&mut stream), Ok(Some(Frame::Pong)))
}

/// One pane the server owns.
struct ServerPane {
    pty: Pty,
    command: String,
    replay: Vec<u8>,
    exited: Option<u32>,
}

/// Everything the event loop reacts to, funneled through one channel.
enum Ev {
    /// A new connection was accepted.
    Conn(u64, UnixStream),
    /// A frame arrived from connection `id`.
    Frame(u64, Frame),
    /// Connection `id` went away.
    Gone(u64),
    /// A pane produced output.
    Out(u64, Vec<u8>),
    /// A pane's child ended.
    Eof(u64),
}

/// Run the session server for `name` until its last pane closes or it is
/// killed. This is the whole process — invoked via the hidden `_server`
/// subcommand, detached from the launching client.
pub fn run(name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("invalid session name: {name:?}"));
    }
    // Detach from the launcher's terminal: a new session means the terminal
    // closing (SIGHUP to its foreground group) can't take the agents down.
    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    let dir = sessions_dir().ok_or("no home directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = socket_path(name).expect("dir resolved");
    match UnixListener::bind(&path) {
        Ok(listener) => serve(name, listener, &path),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if session_alive(name) {
                return Err(format!("session {name} already running"));
            }
            // A stale socket from a dead server: reclaim it.
            std::fs::remove_file(&path).map_err(|e| format!("removing stale socket: {e}"))?;
            let listener = UnixListener::bind(&path)
                .map_err(|e| format!("binding {}: {e}", path.display()))?;
            serve(name, listener, &path)
        }
        Err(e) => Err(format!("binding {}: {e}", path.display())),
    }
}

fn serve(name: &str, listener: UnixListener, path: &std::path::Path) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Ev>();

    // Accept loop: hand every connection to the event loop; its reader
    // thread starts there.
    let accept_tx = tx.clone();
    let accept_listener = listener
        .try_clone()
        .map_err(|e| format!("cloning listener: {e}"))?;
    std::thread::spawn(move || {
        for (conn_id, stream) in (1u64..).zip(accept_listener.incoming()) {
            let Ok(stream) = stream else { break };
            if accept_tx.send(Ev::Conn(conn_id, stream)).is_err() {
                break;
            }
        }
    });

    let result = event_loop(name, &tx, &rx);
    let _ = std::fs::remove_file(path);
    result
}

fn event_loop(name: &str, tx: &Sender<Ev>, rx: &Receiver<Ev>) -> Result<(), String> {
    let mut panes: HashMap<u64, ServerPane> = HashMap::new();
    let mut next_pane: u64 = 1;
    // Agents report hook events to the session socket itself; the frames
    // are relayed to the attached client, which owns detection.
    let hook_sock = socket_path(name).map(|path| path.to_string_lossy().into_owned());
    let mut layout: Vec<u8> = Vec::new();
    // The attached client: connection id + write handle.
    let mut client: Option<(u64, UnixStream)> = None;
    // Connections that exist but haven't attached (probes, fresh attaches).
    let mut conns: HashMap<u64, UnixStream> = HashMap::new();
    let mut ever_spawned = false;

    while let Ok(ev) = rx.recv() {
        match ev {
            Ev::Conn(conn_id, stream) => {
                let Ok(write_half) = stream.try_clone() else {
                    continue;
                };
                conns.insert(conn_id, write_half);
                let frame_tx = tx.clone();
                std::thread::spawn(move || {
                    let mut reader = stream;
                    loop {
                        match read_frame(&mut reader) {
                            Ok(Some(frame)) => {
                                if frame_tx.send(Ev::Frame(conn_id, frame)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) | Err(_) => {
                                let _ = frame_tx.send(Ev::Gone(conn_id));
                                return;
                            }
                        }
                    }
                });
            }
            Ev::Frame(conn_id, frame) => match frame {
                Frame::Ping => {
                    if let Some(mut stream) = conns.remove(&conn_id) {
                        let _ = write_frame(&mut stream, &Frame::Pong);
                        conns.insert(conn_id, stream);
                    }
                }
                Frame::Attach => {
                    let Some(mut stream) = conns.remove(&conn_id) else {
                        continue;
                    };
                    // Takeover: the newest attach wins, the old client is
                    // told why it lost the session.
                    if let Some((_, mut old)) = client.take() {
                        let _ = write_frame(
                            &mut old,
                            &Frame::Shutdown {
                                reason: "another client attached".into(),
                            },
                        );
                    }
                    let hello = Frame::Hello {
                        panes: panes
                            .iter()
                            .map(|(id, p)| HelloPane {
                                pane: *id,
                                command: p.command.clone(),
                                exited: p.exited,
                            })
                            .collect(),
                        layout: layout.clone(),
                    };
                    let mut ok = write_frame(&mut stream, &hello).is_ok();
                    let mut ids: Vec<u64> = panes.keys().copied().collect();
                    ids.sort_unstable();
                    for id in ids {
                        if !ok {
                            break;
                        }
                        ok = write_frame(
                            &mut stream,
                            &Frame::Replay {
                                pane: id,
                                bytes: panes[&id].replay.clone(),
                            },
                        )
                        .is_ok();
                    }
                    if ok {
                        client = Some((conn_id, stream));
                    }
                }
                Frame::Detach => {
                    if client.as_ref().is_some_and(|(id, _)| *id == conn_id) {
                        client = None;
                    }
                    conns.remove(&conn_id);
                }
                Frame::Kill => {
                    if let Some((_, mut stream)) = client.take() {
                        let _ = write_frame(
                            &mut stream,
                            &Frame::Shutdown {
                                reason: format!("session {name} killed"),
                            },
                        );
                    }
                    // Dropping the Ptys kills and reaps every child.
                    panes.clear();
                    return Ok(());
                }
                Frame::Input { pane, bytes } => {
                    if let Some(p) = panes.get_mut(&pane) {
                        if p.exited.is_none() {
                            let _ = p.pty.write(&bytes);
                        }
                    }
                }
                Frame::Resize { pane, cols, rows } => {
                    if let Some(p) = panes.get_mut(&pane) {
                        let _ = p.pty.resize(cols, rows);
                    }
                }
                Frame::Spawn { command } => {
                    let pane_var = next_pane.to_string();
                    let mut env: Vec<(&str, &str)> =
                        vec![(crate::hook::PANE_ENV, pane_var.as_str())];
                    if let Some(sock) = &hook_sock {
                        env.push((crate::hook::SOCK_ENV, sock.as_str()));
                    }
                    match Pty::spawn_with_env(&command, SPAWN_COLS, SPAWN_ROWS, &env) {
                        Ok(pty) => {
                            let pane_id = next_pane;
                            next_pane += 1;
                            ever_spawned = true;
                            start_pane_pump(&pty, pane_id, tx.clone());
                            panes.insert(
                                pane_id,
                                ServerPane {
                                    pty,
                                    command: command.clone(),
                                    replay: Vec::new(),
                                    exited: None,
                                },
                            );
                            send_to_client(
                                &mut client,
                                &Frame::PaneOpened {
                                    pane: pane_id,
                                    command,
                                },
                            );
                        }
                        Err(error) => {
                            send_to_client(
                                &mut client,
                                &Frame::SpawnFailed {
                                    error: error.to_string(),
                                },
                            );
                        }
                    }
                }
                frame @ (Frame::HookBlocked { .. } | Frame::HookClear { .. }) => {
                    // Hook reports relay verbatim to the attached client,
                    // where detection applies them. No client, no harm: the
                    // scrape re-detects the prompt on reattach.
                    send_to_client(&mut client, &frame);
                }
                Frame::Close { pane } => {
                    // Dropping the runtime kills and reaps the child.
                    panes.remove(&pane);
                    if panes.is_empty() && ever_spawned {
                        send_to_client(
                            &mut client,
                            &Frame::Shutdown {
                                reason: "session ended".into(),
                            },
                        );
                        return Ok(());
                    }
                }
                Frame::SetLayout { blob } => layout = blob,
                // Server-side frames from a confused client: ignore.
                _ => {}
            },
            Ev::Gone(conn_id) => {
                conns.remove(&conn_id);
                if client.as_ref().is_some_and(|(id, _)| *id == conn_id) {
                    // Client vanished without a Detach — same thing.
                    client = None;
                }
            }
            Ev::Out(pane_id, bytes) => {
                if let Some(p) = panes.get_mut(&pane_id) {
                    p.replay.extend_from_slice(&bytes);
                    if p.replay.len() > REPLAY_CAP {
                        let excess = p.replay.len() - REPLAY_CAP;
                        p.replay.drain(..excess);
                    }
                    send_to_client(
                        &mut client,
                        &Frame::Output {
                            pane: pane_id,
                            bytes,
                        },
                    );
                }
            }
            Ev::Eof(pane_id) => {
                if let Some(p) = panes.get_mut(&pane_id) {
                    let code = p.pty.wait().map(|status| status.code).unwrap_or(1);
                    p.exited = Some(code);
                    send_to_client(
                        &mut client,
                        &Frame::Exited {
                            pane: pane_id,
                            code,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

/// Pump a pane's output into the event loop.
fn start_pane_pump(pty: &Pty, pane_id: u64, tx: Sender<Ev>) {
    let Ok(mut reader) = pty.reader() else {
        let _ = tx.send(Ev::Eof(pane_id));
        return;
    };
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Ev::Eof(pane_id));
                    return;
                }
                Ok(n) => {
                    if tx.send(Ev::Out(pane_id, buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

/// Write to the attached client, detaching it on failure — a dead pipe must
/// not wedge the session.
fn send_to_client(client: &mut Option<(u64, UnixStream)>, frame: &Frame) {
    if let Some((_, stream)) = client {
        if write_frame(stream, frame).is_err() {
            *client = None;
        }
    }
}
