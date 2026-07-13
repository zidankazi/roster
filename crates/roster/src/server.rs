//! The session server: a background process that owns the PTYs so agents
//! survive the UI. Clients attach over a unix socket, speak
//! `roster-proto`, and can detach and come back — the panes keep running.
//!
//! One client at a time: a new attach takes over and the old client is told
//! to shut down (the tmux model). The server keeps a replay buffer per pane
//! so a reattaching client can rebuild each screen, and stores the client's
//! layout blob verbatim — layout is the client's business.

use std::collections::{HashMap, HashSet};
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
///
/// Private and unvetted on purpose: [`vetted_sessions_dir`] is the only way
/// to reach this path from outside the module, and it vets the dir first.
/// Keeping the raw resolver private is what makes "every socket path is
/// built under a vetted dir" a structural fact rather than a promise in a
/// comment — no caller can resolve a socket path (to probe, connect, list,
/// or unlink) before the dir has been confirmed ours.
fn sessions_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ROSTER_SOCK_DIR") {
        return PathBuf::from(dir);
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/roster-{uid}"))
}

/// The socket file for session `name` inside an already-vetted sessions
/// dir. Takes the dir (from [`vetted_sessions_dir`]) instead of resolving
/// it, so a socket path can't be built before the dir has been vetted.
pub fn socket_in(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.sock"))
}

/// Create-and-vet the sessions dir and hand back its path. This is the one
/// way a command obtains the dir: [`sessions_dir`] is private, so no caller
/// can reach a socket path before the dir is confirmed a real 0700
/// directory owned by us. That closes the world-writable-`/tmp`
/// pre-creation and symlink-swap attacks on the read/attach/list/kill
/// paths, not only the create/bind path.
///
/// Vet-and-create, not vet-only-if-exists: a query like `roster ls` on a
/// host with no sessions leaves an empty 0700 dir behind. That is harmless
/// — it is the same dir a server would create — and it pre-claims the
/// predictable `/tmp` name for the user, so there is nothing left for an
/// attacker to plant there afterward.
pub fn vetted_sessions_dir() -> Result<PathBuf, String> {
    let dir = sessions_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Create `dir` private to the current user (mode 0700), or vet the one
/// already there. The default sessions dir sits in world-writable `/tmp`
/// under a predictable name, so another local user could pre-create it (or
/// plant a symlink) and own every socket dropped inside — and the umask
/// default would leave a fresh one world-readable. A pre-existing path must
/// be a real directory (not a symlink) owned by the current uid; any mode
/// that isn't exactly 0700 is reset (both loose group/world bits and an
/// owner-restrictive mode a socket couldn't bind under), anything else is
/// refused.
pub fn ensure_private_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))?;
    // symlink_metadata: a planted symlink must be seen as itself, not
    // followed to wherever it points.
    let meta =
        std::fs::symlink_metadata(dir).map_err(|e| format!("checking {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid {
        return Err(format!(
            "{} is owned by uid {}, not uid {uid} — refusing to use it",
            dir.display(),
            meta.uid()
        ));
    }
    // Force exactly 0700. Checking only the group/world bits would pass a
    // dir left at 0600/0500 (or a fresh 0700 masked down by an unusual
    // umask), which is private but can't hold a socket — bind would then
    // fail EACCES with no hint why.
    if meta.mode() & 0o7777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("tightening permissions on {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Session names stay path- and shell-safe.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Probe a session socket: `true` when a server answers. `dir` must come
/// from [`vetted_sessions_dir`]: a probe walks the same socket path a later
/// attach would connect to, so it must never run against an unvetted dir.
pub fn session_alive(dir: &std::path::Path, name: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_in(dir, name)) else {
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

    let dir = vetted_sessions_dir()?;
    let path = socket_in(&dir, name);
    match UnixListener::bind(&path) {
        Ok(listener) => serve(name, listener, &path),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if session_alive(&dir, name) {
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

    let result = event_loop(name, path, &tx, &rx);
    let _ = std::fs::remove_file(path);
    result
}

fn event_loop(
    name: &str,
    path: &std::path::Path,
    tx: &Sender<Ev>,
    rx: &Receiver<Ev>,
) -> Result<(), String> {
    let mut panes: HashMap<u64, ServerPane> = HashMap::new();
    let mut next_pane: u64 = 1;
    // Agents report hook events to the session socket itself; the frames
    // are relayed to the attached client, which owns detection.
    let hook_sock = path.to_string_lossy().into_owned();
    let mut layout: Vec<u8> = Vec::new();
    // The attached client: connection id + write handle.
    let mut client: Option<(u64, UnixStream)> = None;
    // Connections that exist but haven't attached (probes, fresh attaches).
    let mut conns: HashMap<u64, UnixStream> = HashMap::new();
    // Panes whose permission asks the server auto-approves, told by the
    // client via `SetAutoApprove`. Keyed by server pane id (the `ROSTER_PANE`
    // value a hook reports), which the client mirrors 1:1.
    let mut auto_approve: HashSet<u64> = HashSet::new();
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
                                // Seed the reattaching client's auto-approve
                                // mirror so it doesn't false-pin blocked panes
                                // the server is silently approving.
                                auto_approve: auto_approve.contains(id),
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
                    let env: Vec<(&str, &str)> = vec![
                        (crate::hook::PANE_ENV, pane_var.as_str()),
                        (crate::hook::SOCK_ENV, hook_sock.as_str()),
                    ];
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
                Frame::HookBlocked { pane, .. } => {
                    // Answer the ask from this pane's auto-approve state, on
                    // the originating hook connection, BEFORE relaying to the
                    // client. Writing first (to a socket `_hook` is actively
                    // reading) keeps the single-threaded loop from stalling
                    // the reply behind a slow client write. Best-effort: an
                    // old or gone `_hook` simply never reads it. No server
                    // read timeout is needed to reap the connection — `_hook`
                    // closes it within its own reply deadline, and a blanket
                    // read timeout would wrongly drop idle attached clients
                    // that share this reader.
                    if let Some(stream) = conns.get_mut(&conn_id) {
                        let allow = auto_approve.contains(&pane);
                        let _ = write_frame(stream, &Frame::HookDecision { allow });
                    }
                    send_to_client(&mut client, &frame);
                }
                Frame::HookClear { .. } | Frame::HookActivity { .. } | Frame::Statusline { .. } => {
                    // Externally-injected frames relay verbatim to the
                    // attached client, where detection applies them. No
                    // client, no harm: the scrape re-detects a prompt on
                    // reattach, and a live agent's statusline feed re-sends.
                    send_to_client(&mut client, &frame);
                }
                Frame::SetAutoApprove { pane, on } => {
                    if on {
                        auto_approve.insert(pane);
                    } else {
                        auto_approve.remove(&pane);
                    }
                }
                Frame::Close { pane } => {
                    // Dropping the runtime kills and reaps the child.
                    panes.remove(&pane);
                    auto_approve.remove(&pane);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// A fresh path under the system temp dir, cleared of leftovers.
    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("roster-dirtest-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn private_dir_is_created_without_group_world_access() {
        let dir = scratch("fresh");
        ensure_private_dir(&dir).expect("create");
        let mode = std::fs::metadata(&dir).expect("stat").mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o} leaks group/world access");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_non_0700_existing_dir_is_reset_to_0700() {
        // Both a looser mode (group/world bits) and an owner-restrictive one
        // (private but too tight to bind a socket under) must land at 0700.
        for pre in [0o755, 0o750, 0o600, 0o500] {
            let dir = scratch(&format!("mode{pre:o}"));
            std::fs::create_dir_all(&dir).expect("pre-create");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(pre)).expect("chmod");
            ensure_private_dir(&dir).expect("vet");
            let mode = std::fs::metadata(&dir).expect("stat").mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "mode {pre:o} was not reset (got {mode:o})"
            );
            std::fs::remove_dir_all(&dir).expect("cleanup");
        }
    }

    #[test]
    fn a_plain_file_at_the_path_is_refused() {
        let path = scratch("file");
        std::fs::write(&path, b"not a dir").expect("write");
        assert!(ensure_private_dir(&path).is_err());
        std::fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn a_symlink_at_the_path_is_refused() {
        let target = scratch("link-target");
        std::fs::create_dir_all(&target).expect("target");
        let link = scratch("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(ensure_private_dir(&link).is_err());
        std::fs::remove_file(&link).expect("cleanup link");
        std::fs::remove_dir_all(&target).expect("cleanup target");
    }
}
