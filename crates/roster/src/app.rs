//! The event loop: PTY output → emulator → detection → model → repaint,
//! plus key handling and the pane-switch side effects.
//!
//! Panes come in two flavors sharing one runtime: local (the app owns the
//! PTY) and remote (a `roster-proto` session server owns it; the app is a
//! view). Remote pane ids mirror the server's, so the layout snapshot the
//! server stores needs no translation on reattach.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roster_proto::{read_frame, write_frame, Frame};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use roster_core::{AgentState, PaneId, Session, SplitDirection};
use roster_detect::{AgentKind, Detector, PaneTracker};
use roster_pty::Pty;
use roster_term::Screen;
use roster_tui::{
    confirm_button_at, confirm_contains, content_rect, exited_buttons, hit_test, launch_items,
    local_panes, panes_area, pointer_for, render, sidebar_entries, toast_rects, ConfirmButton, Hit,
    LaunchItem, Launcher, LauncherState, Message, Pointer, SidebarEntry, SidebarSide, SidebarState,
    ToastLevel, View,
};

use crate::keys::encode_key;

/// How long to wait for input before running another frame.
const INPUT_POLL: Duration = Duration::from_millis(50);
/// How often detection re-reads each pane's screen.
const DETECT_EVERY: Duration = Duration::from_millis(400);
/// How long a hook-reported ask outranks a disagreeing scrape. The hook
/// fires before the prompt paints, so the screen briefly lags it; past
/// this grace, a committed non-blocked scrape means the prompt is gone and
/// the pin is stale (a missed clear — e.g. an interrupt at the prompt).
const HOOK_PIN_GRACE: Duration = Duration::from_secs(2);
/// How close two clicks must land to count as a double-click — matches
/// typical OS double-click defaults; used by the pane-title solo toggle.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Whether a hook-reported ask still outranks the committed scrape: always
/// within the paint grace, and for as long as the settled screen keeps
/// reading blocked (the pin's verbatim ask is richer than the scraped one).
/// Hook events are facts, never fed through the debouncer — this predicate
/// is the single point where the pin and the scrape reconcile.
fn hook_pin_wins(pin_age: Duration, scraped: AgentState) -> bool {
    pin_age < HOOK_PIN_GRACE || scraped == AgentState::Blocked
}
/// Whether the sidebar header's `[auto-yes]` fleet toggle reads armed for
/// these agent cards' auto-approve flags: every card auto-approved, and at
/// least one card. Mirrors the exact condition the header uses to light the
/// toggle (`sidebar.rs`) so an agent spawned while it is lit inherits
/// auto-approve. An empty fleet is never armed — the first agent, spawned
/// into a lit-less header, does not inherit.
fn fleet_auto_armed(card_auto: &[bool]) -> bool {
    !card_auto.is_empty() && card_auto.iter().all(|&on| on)
}
/// Reasons arriving over the hook socket are capped defensively — the
/// `_hook` sender already truncates, but the socket accepts frames from
/// any same-uid process.
const HOOK_REASON_CAP: usize = 160;
/// Size panes are spawned at before the first layout pass corrects them.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

/// Bytes or end-of-stream from a pane's reader thread. The generation says
/// which attachment produced it: replacing a pane's command reuses the pane
/// id, and the dying process's tail (and EOF) must not touch its successor.
enum Output {
    Bytes(PaneId, u64, Vec<u8>),
    Eof(PaneId, u64),
    /// A frame from the hook socket: a Claude Code hook reporting a pane's
    /// exact state, or its statusline feed reporting telemetry (see the
    /// `hook` module).
    Hook(Frame),
}

/// Input interpretation state.
enum Mode {
    /// Keys go to the focused pane; the prefix key arms a command.
    Normal,
    /// The next key is a roster command.
    Prefix,
    /// Arrow/vim keys drive the sidebar selection.
    Jump,
    /// The agent launcher is open and owns typed input.
    Launch(LauncherState),
    /// Closing this pane would kill a live agent; waiting for a yes/no.
    ConfirmClose(PaneId),
}

/// How long a toast stays up before it fades on its own.
const TOAST_TTL: Duration = Duration::from_secs(5);

/// One transient notification card.
struct Toast {
    text: String,
    level: ToastLevel,
    born: Instant,
}

/// A shared write handle to the session server's transport.
type RemoteWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Where a pane's input goes and who owns its process.
enum PaneIo {
    /// The app owns the PTY; dropping it kills the child.
    Local(Pty),
    /// A session server owns the PTY; frames carry input and resizes.
    /// Dropping this does nothing — the pane outlives the client.
    Remote { writer: RemoteWriter, pane: u64 },
}

impl PaneIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            PaneIo::Local(pty) => pty.write(bytes),
            PaneIo::Remote { writer, pane } => {
                let mut w = writer.lock().expect("writer lock");
                write_frame(
                    &mut *w,
                    &Frame::Input {
                        pane: *pane,
                        bytes: bytes.to_vec(),
                    },
                )
            }
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        match self {
            PaneIo::Local(pty) => pty
                .resize(cols, rows)
                .map_err(|e| io::Error::other(e.to_string())),
            PaneIo::Remote { writer, pane } => {
                let mut w = writer.lock().expect("writer lock");
                write_frame(
                    &mut *w,
                    &Frame::Resize {
                        pane: *pane,
                        cols,
                        rows,
                    },
                )
            }
        }
    }
}

/// Where the next server-spawned pane should land in the layout.
enum Placement {
    /// Its own fresh window.
    Window,
    /// Swap in for an existing pane (placeholder replacement, restart).
    Replace(PaneId),
    /// Split an existing pane.
    Split(PaneId, SplitDirection),
}

/// The client side of a persistent session.
struct RemoteSession {
    /// The session's name, for detach messaging.
    name: String,
    /// Serialized writes to the server.
    writer: RemoteWriter,
    /// Frames from the server, pumped by a reader thread.
    rx: Receiver<Frame>,
    /// Spawn placements awaiting their `PaneOpened`, in request order.
    pending: VecDeque<Placement>,
    /// The last layout blob sent, to skip redundant `SetLayout`s.
    last_layout: String,
    /// Set when the user detached (vs the session ending).
    detached: bool,
    /// The server's shutdown reason, when it ended the connection.
    shutdown: Option<String>,
}

impl RemoteSession {
    fn send(&self, frame: &Frame) {
        let mut w = self.writer.lock().expect("writer lock");
        let _ = write_frame(&mut *w, frame);
    }
}

/// A hook-reported permission ask pinning a pane to blocked.
struct HookPin {
    /// The tool the ask is about, for matching the eventual clear.
    tool: String,
    /// The verbatim ask, shown as the sidebar reason.
    reason: String,
    /// When the pin landed, for the reconciliation grace period.
    at: Instant,
}

/// Everything one live pane owns besides its model entry.
struct PaneRuntime {
    io: PaneIo,
    screen: Screen,
    tracker: PaneTracker,
    kind: Option<AgentKind>,
    /// Which attachment this is; output from older generations is stale.
    generation: u64,
    /// Exit code, once the child has ended; the pane stays visible.
    exited: Option<u32>,
    /// A hook-reported permission ask. While set, the pane reads blocked
    /// with this exact reason. Cleared by the hook's `PreToolUse`/`Stop`
    /// events — or by the scrape itself, when a settled screen no longer
    /// shows any prompt (see [`HOOK_PIN_GRACE`]): the hook wins on
    /// freshness and richness, the screen wins on reality.
    hook_blocked: Option<HookPin>,
}

/// The running multiplexer.
pub struct App {
    session: Session,
    runtimes: HashMap<PaneId, PaneRuntime>,
    detector: Detector,
    sidebar: SidebarState,
    side: SidebarSide,
    mode: Mode,
    launchables: Vec<LaunchItem>,
    last_entries: Vec<SidebarEntry>,
    last_detect: Instant,
    /// The frame area of the most recent draw, for mouse hit-testing.
    last_area: Rect,
    /// A grabbed split divider, in pane-local coordinates, while dragging.
    dragging: Option<(u16, u16)>,
    /// The bare-start shell pane: a backdrop for the launcher only. The
    /// first launch replaces it unconditionally, so a plain shell never
    /// survives as its own workspace — it is scenery, not a tenant.
    placeholder: Option<PaneId>,
    /// Attachment counter feeding [`PaneRuntime::generation`].
    next_generation: u64,
    /// Solo view: show only the focused pane; the sidebar switches.
    zoomed: bool,
    /// The last known mouse position, for hover affordances.
    last_mouse: Option<(u16, u16)>,
    /// The previous left-click, for double-click detection on titles.
    last_click: Option<(Instant, (u16, u16))>,
    /// The pointer shape most recently told to the terminal.
    pointer: Pointer,
    /// Transient notification cards, newest first.
    toasts: Vec<Toast>,
    /// A mouse-drag selection being made: the pane and the anchor cell, in
    /// pane-content coordinates.
    sel_anchor: Option<(PaneId, (u16, u16))>,
    /// The current selection: pane and both endpoints, content-local.
    /// Highlighted until the next click or keystroke; copied on release.
    selection: Option<roster_tui::Selection>,
    /// The persistent session this app is attached to, if any.
    remote: Option<RemoteSession>,
    quit: bool,
    output_tx: Sender<Output>,
    output_rx: Receiver<Output>,
    /// The hook socket local panes are told about via `ROSTER_HOOK_SOCK`.
    /// `None` in remote mode (the session server owns the hook socket) or
    /// when the listener could not start (scraping still works).
    hook_sock: Option<PathBuf>,
    /// Panes whose permission asks roster auto-approves. Shared with the
    /// hook-listener thread, which answers each ask from it; toggled from
    /// the sidebar. In remote mode the session server owns the authoritative
    /// set (told via `SetAutoApprove`) and this local copy only drives the
    /// card's `auto` chip. Default empty — auto-approve is opt-in per pane.
    auto_approve: Arc<Mutex<HashSet<u64>>>,
}

impl App {
    /// Spawn one pane per command and assemble the initial layout,
    /// alternating split directions for a usable mosaic. With
    /// `open_launcher`, the agent launcher opens over the first frame — the
    /// bare-`roster` greeting.
    pub fn new(
        detector: Detector,
        commands: &[String],
        side: SidebarSide,
        open_launcher: bool,
    ) -> Result<App, String> {
        let (output_tx, output_rx) = mpsc::channel();
        let auto_approve: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let hook_sock = start_hook_listener(output_tx.clone(), Arc::clone(&auto_approve));
        let launchables = launch_items(&detector);
        let mut app = App {
            session: Session::new(),
            runtimes: HashMap::new(),
            detector,
            sidebar: SidebarState::new(),
            side,
            mode: if open_launcher {
                Mode::Launch(LauncherState::new())
            } else {
                Mode::Normal
            },
            launchables,
            last_entries: Vec::new(),
            last_detect: Instant::now() - DETECT_EVERY,
            last_area: Rect::new(0, 0, SPAWN_COLS, SPAWN_ROWS),
            dragging: None,
            placeholder: None,
            next_generation: 0,
            zoomed: false,
            last_mouse: None,
            last_click: None,
            pointer: Pointer::Default,
            toasts: Vec::new(),
            sel_anchor: None,
            selection: None,
            remote: None,
            quit: false,
            output_tx,
            output_rx,
            hook_sock,
            auto_approve,
        };

        let first = app.session.focused().expect("new session has a pane");
        app.attach(first, &commands[0])
            .map_err(|e| format!("spawning `{}`: {e}", commands[0]))?;
        if open_launcher {
            app.placeholder = Some(first);
        }
        for (i, command) in commands.iter().enumerate().skip(1) {
            let direction = if i % 2 == 1 {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            let target = app.session.focused().expect("session is non-empty");
            let id = app
                .session
                .split(target, direction)
                .expect("focused pane exists");
            app.attach(id, command)
                .map_err(|e| format!("spawning `{command}`: {e}"))?;
        }
        Ok(app)
    }

    /// Attach to a persistent session over `reader`/`writer` (a unix
    /// socket locally, an ssh subprocess's stdio remotely). Rebuilds the
    /// model from the server's `Hello` and stored layout; pane screens
    /// repaint from replay frames. Extra `commands` each get a fresh
    /// window; a brand-new session with none opens the welcome launcher
    /// over a placeholder shell, like a bare local start.
    pub fn new_remote(
        detector: Detector,
        side: SidebarSide,
        name: &str,
        mut reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        commands: &[String],
    ) -> Result<App, String> {
        let mut writer = writer;
        write_frame(&mut writer, &Frame::Attach).map_err(|e| format!("attaching: {e}"))?;
        let hello = read_frame(&mut reader)
            .map_err(|e| format!("reading hello: {e}"))?
            .ok_or("server closed the connection")?;
        let Frame::Hello { mut panes, layout } = hello else {
            return Err("server spoke out of turn".to_string());
        };
        panes.sort_by_key(|p| p.pane);

        // The stored layout is the truth about shape; the Hello about
        // existence. Panes the server dropped leave the layout, panes the
        // layout never saw get their own windows.
        let mut session = std::str::from_utf8(&layout)
            .ok()
            .and_then(Session::restore)
            .unwrap_or_else(Session::empty);
        let known: Vec<PaneId> = session.panes().iter().map(|p| p.id).collect();
        for id in known {
            if !panes.iter().any(|p| p.pane == id.raw()) {
                session.close(id);
            }
        }
        for pane in &panes {
            if session.pane(PaneId::from_raw(pane.pane)).is_none() {
                session.adopt_window(pane.pane);
            }
        }

        // Bootstrap synchronously (no reader thread yet): a fresh session
        // spawns its placeholder shell or the requested commands, so the
        // model is never empty when the loop starts. Frames other than the
        // awaited PaneOpened are deferred into the channel below.
        let (frame_tx, frame_rx) = mpsc::channel::<Frame>();
        let mut placeholder = None;
        let mut bootstrap: Vec<(u64, String)> = Vec::new();
        if session.is_empty() {
            let to_spawn: Vec<String> = if commands.is_empty() {
                vec![default_shell()]
            } else {
                commands.to_vec()
            };
            for command in &to_spawn {
                write_frame(
                    &mut writer,
                    &Frame::Spawn {
                        command: command.clone(),
                    },
                )
                .map_err(|e| format!("spawning {command}: {e}"))?;
            }
            let mut opened = 0;
            while opened < to_spawn.len() {
                match read_frame(&mut reader).map_err(|e| format!("attaching: {e}"))? {
                    Some(Frame::PaneOpened { pane, command }) => {
                        let id = session
                            .adopt_window(pane)
                            .ok_or("server reused a pane id")?;
                        bootstrap.push((pane, command));
                        if commands.is_empty() {
                            placeholder = Some(id);
                        }
                        opened += 1;
                    }
                    Some(Frame::SpawnFailed { error }) => {
                        return Err(format!("spawn failed: {error}"));
                    }
                    Some(other) => {
                        let _ = frame_tx.send(other);
                    }
                    None => return Err("server closed during attach".to_string()),
                }
            }
        }

        // From here frames flow through the channel.
        let pump_tx = frame_tx.clone();
        std::thread::spawn(move || loop {
            match read_frame(&mut reader) {
                Ok(Some(frame)) => {
                    if pump_tx.send(frame).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = pump_tx.send(Frame::Shutdown {
                        reason: "connection closed".into(),
                    });
                    return;
                }
                Err(error) => {
                    let _ = pump_tx.send(Frame::Shutdown {
                        reason: format!("connection lost: {error}"),
                    });
                    return;
                }
            }
        });

        let (output_tx, output_rx) = mpsc::channel();
        let launchables = launch_items(&detector);
        let mut app = App {
            session,
            runtimes: HashMap::new(),
            detector,
            sidebar: SidebarState::new(),
            side,
            mode: if placeholder.is_some() {
                Mode::Launch(LauncherState::new())
            } else {
                Mode::Normal
            },
            launchables,
            last_entries: Vec::new(),
            last_detect: Instant::now() - DETECT_EVERY,
            last_area: Rect::new(0, 0, SPAWN_COLS, SPAWN_ROWS),
            dragging: None,
            placeholder,
            next_generation: 0,
            zoomed: false,
            last_mouse: None,
            last_click: None,
            pointer: Pointer::Default,
            toasts: Vec::new(),
            sel_anchor: None,
            selection: None,
            remote: Some(RemoteSession {
                name: name.to_string(),
                writer: Arc::new(Mutex::new(writer)),
                rx: frame_rx,
                pending: VecDeque::new(),
                last_layout: String::new(),
                detached: false,
                shutdown: None,
            }),
            quit: false,
            output_tx,
            output_rx,
            // The session server owns the panes and their hook socket;
            // hook frames arrive relayed over the session connection.
            hook_sock: None,
            // Seed the auto-approve mirror from the server's `Hello` so a
            // reattach doesn't false-pin (or unlight the `auto` chip on) panes
            // the server keeps silently approving. The server stays the
            // authoritative set (told via `SetAutoApprove`); this local copy
            // drives the card chip and blocked-pin suppression.
            auto_approve: Arc::new(Mutex::new(
                panes
                    .iter()
                    .filter(|p| p.auto_approve)
                    .map(|p| p.pane)
                    .collect(),
            )),
        };
        for pane in &panes {
            app.attach_remote_pane(PaneId::from_raw(pane.pane), pane.pane, &pane.command);
        }
        let bootstrapped = !bootstrap.is_empty();
        for (pane, command) in bootstrap {
            app.attach_remote_pane(PaneId::from_raw(pane), pane, &command);
        }
        for pane in &panes {
            if let Some(code) = pane.exited {
                app.mark_exited_with_code(PaneId::from_raw(pane.pane), code);
            }
        }
        // Commands given against an already-populated session each get
        // their own window, like launching them by hand.
        if !bootstrapped && !panes.is_empty() {
            for command in commands {
                if let Some(remote) = &mut app.remote {
                    remote.pending.push_back(Placement::Window);
                    remote.send(&Frame::Spawn {
                        command: command.clone(),
                    });
                }
            }
        }
        Ok(app)
    }

    /// The message to print after the loop ends, for session runs: how the
    /// attachment ended and how to come back.
    pub fn exit_message(&self) -> Option<String> {
        let remote = self.remote.as_ref()?;
        if remote.detached {
            Some(format!(
                "detached — reattach with: roster attach {}",
                remote.name
            ))
        } else {
            remote.shutdown.clone()
        }
    }

    /// Attach a freshly spawned command to an existing model pane and start
    /// its reader thread.
    fn attach(&mut self, id: PaneId, command: &str) -> Result<(), String> {
        if self.selection.map(|s| s.0) == Some(id) {
            self.selection = None;
        }
        // Every pane learns its identity and the hook socket, so a claude
        // running in it can report exact state back through its hooks.
        // (Reports render only for identified claude panes — see
        // apply_hook_blocked — but injecting everywhere is harmless and
        // keeps the spawn path uniform.)
        let pane_var = id.raw().to_string();
        let sock_var = self
            .hook_sock
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let mut env: Vec<(&str, &str)> = vec![(crate::hook::PANE_ENV, pane_var.as_str())];
        if let Some(sock) = &sock_var {
            env.push((crate::hook::SOCK_ENV, sock.as_str()));
        }
        let pty = Pty::spawn_with_env(command, SPAWN_COLS, SPAWN_ROWS, &env)
            .map_err(|e| e.to_string())?;
        let mut reader = pty.reader().map_err(|e| e.to_string())?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let tx = self.output_tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Output::Eof(id, generation));
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(Output::Bytes(id, generation, buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        self.runtimes.insert(
            id,
            PaneRuntime {
                io: PaneIo::Local(pty),
                screen: Screen::new(SPAWN_COLS, SPAWN_ROWS),
                tracker: PaneTracker::new(),
                kind: self.detector.identify(command),
                generation,
                exited: None,
                hook_blocked: None,
            },
        );
        if let Some(pane) = self.session.pane_mut(id) {
            pane.command = Some(command.to_string());
        }
        self.inherit_fleet_auto_approve(id, command);
        Ok(())
    }

    /// Register a server-owned pane: model command, emulator screen, agent
    /// detection. The pane id mirrors the server's.
    fn attach_remote_pane(&mut self, id: PaneId, server_pane: u64, command: &str) {
        let writer = self
            .remote
            .as_ref()
            .expect("remote pane outside a session")
            .writer
            .clone();
        if self.selection.map(|s| s.0) == Some(id) {
            self.selection = None;
        }
        self.runtimes.insert(
            id,
            PaneRuntime {
                io: PaneIo::Remote {
                    writer,
                    pane: server_pane,
                },
                screen: Screen::new(SPAWN_COLS, SPAWN_ROWS),
                tracker: PaneTracker::new(),
                kind: self.detector.identify(command),
                hook_blocked: None,
                generation: 0,
                exited: None,
            },
        );
        if let Some(pane) = self.session.pane_mut(id) {
            pane.command = Some(command.to_string());
        }
        self.inherit_fleet_auto_approve(id, command);
    }

    /// Drive the loop until quit or every pane is gone.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let started = Instant::now();
        // Claim the pointer up front: without this the terminal keeps its
        // native I-beam over the whole app until the first hover writes a
        // shape. OSC 22 "default" is the plain arrow.
        {
            use std::io::Write;
            let mut out = io::stdout();
            let _ = write!(out, "\x1b]22;{}\x07", Pointer::Default.name());
            let _ = out.flush();
        }
        while !self.quit && !self.session.is_empty() {
            self.drain_output();
            self.drain_remote();
            if self.quit || self.session.is_empty() {
                break;
            }
            let size = terminal.size()?;
            self.last_area = Rect::new(0, 0, size.width, size.height);
            self.sync_layout(self.last_area);
            self.detect_if_due();
            self.toasts.retain(|toast| toast.born.elapsed() < TOAST_TTL);
            self.sync_remote_layout();

            self.last_entries = sidebar_entries(&self.session, &self.detector, Instant::now());
            // Light the `auto` chip from the shared set (a poisoned lock
            // just leaves chips unlit). Different fields than last_entries, so
            // the borrows are disjoint.
            if let Ok(set) = self.auto_approve.lock() {
                for entry in &mut self.last_entries {
                    entry.auto_approve = set.contains(&entry.pane.raw());
                }
            }
            let grids: HashMap<_, _> = self
                .runtimes
                .iter()
                .map(|(id, rt)| (*id, rt.screen.grid()))
                .collect();
            let exited: HashMap<_, _> = self
                .runtimes
                .iter()
                .filter_map(|(id, rt)| rt.exited.map(|code| (*id, code)))
                .collect();
            let selected = match self.mode {
                Mode::Jump => self.sidebar.selected(&self.last_entries),
                _ => None,
            };
            // Hover follows the last known pointer position; the launcher
            // and confirm modals own hover themselves while open.
            let hover = match self.mode {
                Mode::Launch(_) | Mode::ConfirmClose(_) => None,
                _ => self.last_mouse.map(|(x, y)| self.hit_at(x, y)),
            };
            let (mode_badge, status) = self.status_line();
            let launcher = match &self.mode {
                Mode::Launch(state) => Some((self.launchables.as_slice(), state)),
                _ => None,
            };
            let confirm = match &self.mode {
                Mode::ConfirmClose(_) => {
                    let hover = self
                        .last_mouse
                        .and_then(|(x, y)| confirm_button_at(self.last_area, x, y));
                    Some(hover)
                }
                _ => None,
            };
            let toast_view: Vec<(&str, ToastLevel)> = self
                .toasts
                .iter()
                .map(|toast| (toast.text.as_str(), toast.level))
                .collect();
            let scrolled: HashMap<PaneId, usize> = self
                .runtimes
                .iter()
                .filter_map(|(id, rt)| {
                    let offset = rt.screen.display_offset();
                    (offset > 0).then_some((*id, offset))
                })
                .collect();
            let view = View {
                session: &self.session,
                grids: &grids,
                exited: &exited,
                entries: &self.last_entries,
                selected,
                hover,
                zoomed: self.zoomed,
                side: self.side,
                launcher,
                confirm,
                toasts: &toast_view,
                selection: self.selection,
                scrolled: &scrolled,
                welcome: self.placeholder.is_some(),
                mode_badge,
                status: &status,
                // ~8 spinner frames per second, derived from wall time so
                // the cadence is stable regardless of input polling.
                tick: started.elapsed().as_millis() as u64 / 125,
            };
            terminal.draw(|frame| render(frame, &view))?;

            if event::poll(INPUT_POLL)? {
                // Drain the whole burst: mouse motion arrives far faster
                // than one event per frame, and queueing behind redraws
                // would make hover feel laggy.
                loop {
                    match event::read()? {
                        Event::Key(key) if key.kind != KeyEventKind::Release => {
                            self.handle_key(key);
                        }
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        Event::Paste(text) => self.handle_paste(&text),
                        // Resizes are picked up from terminal.size() next frame.
                        _ => {}
                    }
                    if self.quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn drain_output(&mut self) {
        while let Ok(message) = self.output_rx.try_recv() {
            match message {
                Output::Bytes(id, generation, bytes) => {
                    if let Some(rt) = self.runtimes.get_mut(&id) {
                        if rt.generation == generation {
                            rt.screen.advance(&bytes);
                        }
                    }
                }
                Output::Eof(id, generation) => {
                    if self
                        .runtimes
                        .get(&id)
                        .is_some_and(|rt| rt.generation == generation)
                    {
                        self.mark_exited(id);
                    }
                }
                Output::Hook(Frame::HookBlocked { pane, tool, reason }) => {
                    self.apply_hook_blocked(pane, tool, reason);
                }
                Output::Hook(Frame::HookClear { pane, tool }) => {
                    self.apply_hook_clear(pane, &tool);
                }
                Output::Hook(Frame::Statusline { pane, json }) => {
                    self.apply_statusline(pane, &json);
                }
                Output::Hook(_) => {}
            }
        }
    }

    /// A hook reported a permission ask: pin the pane to blocked with the
    /// verbatim reason, immediately — no scrape, no debounce. Only panes
    /// running an identified agent take pins: nothing renders state for
    /// other panes, so a pin there would just be stale model state.
    fn apply_hook_blocked(&mut self, pane: u64, tool: String, reason: String) {
        // An auto-approved pane's ask is answered without the human, so it
        // must not pin 🔴 blocked: that would demand attention for something
        // already waved through, and — since no prompt ever paints — the
        // scrape has nothing to reconcile against, so the paint grace would
        // hold a false blocked. The lit `auto` chip is the pane's sidebar
        // signal; the scrape keeps its real (working) state.
        if self.is_auto_approve(pane) {
            return;
        }
        let id = PaneId::from_raw(pane);
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.kind.is_none() || rt.exited.is_some() {
            return;
        }
        let mut reason = reason;
        if reason.chars().count() > HOOK_REASON_CAP {
            reason = reason.chars().take(HOOK_REASON_CAP).collect();
        }
        rt.hook_blocked = Some(HookPin {
            tool,
            reason: reason.clone(),
            at: Instant::now(),
        });
        self.session
            .set_reading(id, AgentState::Blocked, Some(reason), Instant::now());
    }

    /// A statusline payload arrived for a pane: parse it with the pinned
    /// contract in `roster-detect` and hand the telemetry to the pane's
    /// tracker, which owns freshness and staleness (docs/05 Phase 2). The
    /// socket accepts frames from any local process, so payloads are
    /// advisory until they parse — junk yields `None` and is dropped. Same
    /// gate as hook pins: only identified, live agent panes take telemetry;
    /// nothing renders it for other panes.
    fn apply_statusline(&mut self, pane: u64, json: &str) {
        // Cheap gates before the parse: a payload for a dead, unidentified,
        // or unknown pane must not pay a JSON parse on the loop thread. The
        // size cap is the sender's own stdin cap, shared so the two can't
        // drift apart.
        let Some(rt) = self.runtimes.get_mut(&PaneId::from_raw(pane)) else {
            return;
        };
        if rt.kind.is_none() || rt.exited.is_some() {
            return;
        }
        if json.len() as u64 > crate::hook::MAX_PAYLOAD {
            return;
        }
        let Some(telemetry) = roster_detect::statusline::parse(json) else {
            return;
        };
        rt.tracker.set_telemetry(telemetry, Instant::now());
    }

    /// A clear event: `tool` names the ask it answers (an approved tool's
    /// `PreToolUse`), or is empty to clear any ask (end of turn). A clear
    /// for a different tool is someone else's business — a parallel
    /// auto-approved tool must not erase a still-pending ask.
    fn apply_hook_clear(&mut self, pane: u64, tool: &str) {
        if let Some(rt) = self.runtimes.get_mut(&PaneId::from_raw(pane)) {
            let matches = rt
                .hook_blocked
                .as_ref()
                .is_some_and(|pin| tool.is_empty() || pin.tool == tool);
            if matches {
                rt.hook_blocked = None;
            }
        }
    }

    /// Arm a freshly spawned agent pane when the fleet `[auto-yes]` toggle is
    /// on, so creating an agent while it is lit doesn't silently un-light it —
    /// the newcomer joins auto-approved. Only identified agents inherit: a
    /// plain shell has no asks and never carries the chip. Reads `last_entries`
    /// (the previous frame's cards), so the just-spawned pane is excluded from
    /// the armed check and reattach — where `last_entries` is still empty —
    /// never triggers it. Mirrors `toggle_auto_approve`'s server sync so the
    /// agent is auto-approved from its first ask.
    fn inherit_fleet_auto_approve(&mut self, id: PaneId, command: &str) {
        if self.detector.identify(command).is_none() {
            return;
        }
        let card_auto: Vec<bool> = self.last_entries.iter().map(|e| e.auto_approve).collect();
        if !fleet_auto_armed(&card_auto) {
            return;
        }
        let raw = id.raw();
        let inserted = self
            .auto_approve
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(raw);
        // In a session the server owns the authoritative set; standalone, the
        // local hook listener reads this same shared set, so the insert alone
        // suffices and `remote_send` is a no-op.
        if inserted {
            self.remote_send(&Frame::SetAutoApprove {
                pane: raw,
                on: true,
            });
        }
    }

    /// Whether `pane`'s asks are auto-approved. Poison-tolerant: a poisoned
    /// lock reads as not auto-approved (safe — the human is asked).
    fn is_auto_approve(&self, pane: u64) -> bool {
        self.auto_approve
            .lock()
            .map(|set| set.contains(&pane))
            .unwrap_or(false)
    }

    /// Flip auto-approve for `pane` and toast the new state — the shared
    /// tail of the chip click and jump-mode `a`, so mouse and keyboard
    /// feedback can't drift apart. In a session the server owns the
    /// authoritative set (told via `SetAutoApprove`); the local set still
    /// drives the card chip. Recovers a poisoned lock rather than
    /// panicking — losing the whole hook path would be far worse than a
    /// stale toggle.
    fn toggle_auto_approve(&mut self, pane: u64) {
        let now_on = {
            let mut set = self
                .auto_approve
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if set.remove(&pane) {
                false
            } else {
                set.insert(pane);
                true
            }
        };
        self.remote_send(&Frame::SetAutoApprove { pane, on: now_on });
        // The toast names the pane — its title (agent CLIs put their task
        // there) over the generic agent name: cards re-sort as states
        // change, so the feedback must say which agent actually toggled.
        let id = PaneId::from_raw(pane);
        let name = self
            .runtimes
            .get(&id)
            .and_then(|rt| rt.screen.title())
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| self.pane_name(id));
        self.toast(
            format!(
                "auto-approve {} — {name}",
                if now_on { "on" } else { "off" }
            ),
            ToastLevel::Info,
        );
    }

    /// The fleet toggle behind the sidebar header's `auto-yes` button: arm
    /// auto-approve for every agent pane, or — when the whole fleet is
    /// already armed — disarm all. Every pane is told the target state
    /// (idempotent on the server), and one toast summarizes the sweep.
    fn toggle_auto_all(&mut self) {
        let panes: Vec<u64> = self.last_entries.iter().map(|e| e.pane.raw()).collect();
        if panes.is_empty() {
            return;
        }
        let arm = !panes.iter().all(|pane| self.is_auto_approve(*pane));
        {
            let mut set = self
                .auto_approve
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for pane in &panes {
                if arm {
                    set.insert(*pane);
                } else {
                    set.remove(pane);
                }
            }
        }
        for pane in &panes {
            self.remote_send(&Frame::SetAutoApprove {
                pane: *pane,
                on: arm,
            });
        }
        self.toast(
            format!(
                "auto-approve {} for all {} agent{}",
                if arm { "on" } else { "off" },
                panes.len(),
                if panes.len() == 1 { "" } else { "s" }
            ),
            ToastLevel::Info,
        );
    }

    /// Apply frames from the session server: output repaints screens,
    /// exits linger, opened panes land per their queued placement, and a
    /// shutdown ends the client (the panes live on).
    fn drain_remote(&mut self) {
        let frames: Vec<Frame> = match &self.remote {
            Some(remote) => remote.rx.try_iter().collect(),
            None => return,
        };
        for frame in frames {
            match frame {
                Frame::Output { pane, bytes } | Frame::Replay { pane, bytes } => {
                    if let Some(rt) = self.runtimes.get_mut(&PaneId::from_raw(pane)) {
                        rt.screen.advance(&bytes);
                    }
                }
                Frame::Exited { pane, code } => {
                    self.mark_exited_with_code(PaneId::from_raw(pane), code);
                }
                Frame::PaneOpened { pane, command } => self.place_opened_pane(pane, &command),
                Frame::HookBlocked { pane, tool, reason } => {
                    self.apply_hook_blocked(pane, tool, reason)
                }
                Frame::HookClear { pane, tool } => self.apply_hook_clear(pane, &tool),
                Frame::Statusline { pane, json } => self.apply_statusline(pane, &json),
                Frame::SpawnFailed { error } => {
                    if let Some(remote) = &mut self.remote {
                        remote.pending.pop_front();
                    }
                    self.toast(format!("launch failed: {error}"), ToastLevel::Error);
                }
                Frame::Shutdown { reason } => {
                    if let Some(remote) = &mut self.remote {
                        if !remote.detached {
                            remote.shutdown = Some(reason);
                        }
                    }
                    self.quit = true;
                }
                _ => {}
            }
        }
    }

    /// Land a server-spawned pane where its `Spawn` asked it to go.
    fn place_opened_pane(&mut self, server_pane: u64, command: &str) {
        let placement = self
            .remote
            .as_mut()
            .and_then(|remote| remote.pending.pop_front())
            .unwrap_or(Placement::Window);
        let id = match placement {
            Placement::Window => self.session.adopt_window(server_pane),
            Placement::Replace(old) => {
                let new = self.session.replace_pane(old, server_pane);
                if new.is_some() {
                    // The replaced pane dies with its server twin.
                    if let Some(rt) = self.runtimes.remove(&old) {
                        if let PaneIo::Remote { pane, .. } = rt.io {
                            self.remote_send(&Frame::Close { pane });
                        }
                    }
                    if self.placeholder == Some(old) {
                        self.placeholder = None;
                    }
                    new
                } else {
                    // The target vanished while the spawn was in flight;
                    // give the new pane its own window instead.
                    self.session.adopt_window(server_pane)
                }
            }
            Placement::Split(target, direction) => self
                .session
                .adopt_split(target, server_pane, direction)
                .or_else(|| self.session.adopt_window(server_pane)),
        };
        if let Some(id) = id {
            self.attach_remote_pane(id, server_pane, command);
        }
    }

    fn remote_send(&self, frame: &Frame) {
        if let Some(remote) = &self.remote {
            remote.send(frame);
        }
    }

    /// Push the session's shape to the server when it changes, so the next
    /// attach rebuilds the same layout.
    fn sync_remote_layout(&mut self) {
        if self.remote.is_none() || self.session.is_empty() {
            return;
        }
        let blob = self.session.snapshot();
        if let Some(remote) = &mut self.remote {
            if remote.last_layout != blob {
                remote.send(&Frame::SetLayout {
                    blob: blob.clone().into_bytes(),
                });
                remote.last_layout = blob;
            }
        }
    }

    /// Leave a persistent session running and end the client.
    fn detach(&mut self) {
        if self.remote.is_none() {
            self.toast(
                "no session to detach — start with roster -s NAME".into(),
                ToastLevel::Info,
            );
            return;
        }
        let blob = self.session.snapshot();
        if let Some(remote) = &mut self.remote {
            remote.send(&Frame::SetLayout {
                blob: blob.into_bytes(),
            });
            remote.send(&Frame::Detach);
            remote.detached = true;
        }
        self.quit = true;
    }

    /// The pane's process ended: keep its final screen on display with an
    /// exited notice, and stop treating it as a live agent.
    fn mark_exited(&mut self, id: PaneId) {
        let code = match self.runtimes.get_mut(&id) {
            Some(rt) => match &mut rt.io {
                PaneIo::Local(pty) => match pty.try_wait() {
                    Ok(Some(status)) => status.code,
                    _ => 1,
                },
                // Remote exits arrive with their code in the frame.
                PaneIo::Remote { .. } => return,
            },
            None => return,
        };
        self.mark_exited_with_code(id, code);
    }

    fn mark_exited_with_code(&mut self, id: PaneId, code: u32) {
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.exited.is_some() {
            return;
        }
        rt.exited = Some(code);
        rt.kind = None;
        // Release any done latch first: an exit is its own signal, so the
        // exited reason/state must land instead of being masked by a stale
        // 🔵 done the human never looked at.
        self.session.mark_seen(id);
        self.session.set_reading(
            id,
            AgentState::Idle,
            Some(format!("exited ({code})")),
            Instant::now(),
        );
        // Clearing kind takes the pane out of the detection loop — the only
        // other place telemetry is written — so without this the dead
        // pane's badges would freeze at their last numbers forever.
        self.session.set_telemetry(id, None);
    }

    /// The solo pane, when solo view is active: it follows focus.
    fn zoomed_pane(&self) -> Option<PaneId> {
        if self.zoomed {
            self.session.focused()
        } else {
            None
        }
    }

    /// Bring every pane's PTY and emulator to its laid-out size. In solo
    /// view only the shown pane is resized; hidden panes keep their last
    /// size until they are shown again.
    fn sync_layout(&mut self, area: Rect) {
        let panes = panes_area(area, self.side);
        let local = local_panes(panes);
        let layout = match self.zoomed_pane() {
            Some(id) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
            None => self.session.layout(panes.width, panes.height),
        };
        for (id, rect) in layout {
            let content = content_rect(rect, local);
            if content.width == 0 || content.height == 0 {
                continue;
            }
            let Some(rt) = self.runtimes.get_mut(&id) else {
                continue;
            };
            if rt.screen.size() != (content.width, content.height) {
                rt.screen.resize(content.width, content.height);
                let _ = rt.io.resize(content.width, content.height);
            }
        }
    }

    fn detect_if_due(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_detect) < DETECT_EVERY {
            return;
        }
        self.last_detect = now;
        // Focusing a pane is the acknowledgment that clears its done latch;
        // polled here because focus changes in many places and this is the
        // one tick everything flows through.
        if let Some(focused) = self.session.focused() {
            self.session.mark_seen(focused);
        }
        for (id, rt) in &mut self.runtimes {
            let Some(kind) = rt.kind else {
                continue;
            };
            // The scrape always runs — it is the reconciliation signal and
            // the fallback, never suspended.
            let grid = rt.screen.grid();
            let reading = rt.tracker.update(&self.detector, kind, &grid, now);
            // Telemetry rides every reading — pinned-blocked panes too —
            // and a `None` clears the model, mirroring the tracker's aging
            // instead of freezing the last numbers on screen.
            self.session.set_telemetry(*id, reading.telemetry);
            // The live terminal title labels the pane's sidebar card; a
            // reset title clears it so the card falls back to the agent
            // name rather than a stale task.
            let title = rt
                .screen
                .title()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            self.session.set_title(*id, title);
            // The hook wins on freshness and richness, the screen wins on
            // settled reality — a missed clear (an interrupt at the prompt
            // fires no hook) self-heals here. See `hook_pin_wins`.
            if let Some(pin) = &rt.hook_blocked {
                if hook_pin_wins(now.duration_since(pin.at), reading.state) {
                    // set_reading only bumps last_change on a state change,
                    // so re-pinning keeps the blocked-for age honest.
                    self.session.set_reading(
                        *id,
                        AgentState::Blocked,
                        Some(pin.reason.clone()),
                        now,
                    );
                    continue;
                }
                rt.hook_blocked = None;
            }
            self.session
                .set_reading(*id, reading.state, reading.reason, now);
        }
    }

    /// The index of the toast under (`x`, `y`), when one is there.
    fn toast_at(&self, x: u16, y: u16) -> Option<usize> {
        let toast_view: Vec<(&str, ToastLevel)> = self
            .toasts
            .iter()
            .map(|toast| (toast.text.as_str(), toast.level))
            .collect();
        toast_rects(self.last_area, &toast_view)
            .iter()
            .position(|rect| {
                rect.height > 0
                    && x >= rect.x
                    && x < rect.x + rect.width
                    && y >= rect.y
                    && y < rect.y + rect.height
            })
    }

    /// Show a transient toast card.
    fn toast(&mut self, text: String, level: ToastLevel) {
        self.toasts.insert(
            0,
            Toast {
                text,
                level,
                born: Instant::now(),
            },
        );
        self.toasts.truncate(4);
    }

    /// A pane's absolute content rect, mirroring render's geometry — solo
    /// view fills the pane region, the grid otherwise. `None` when the pane
    /// isn't visible in the active window.
    fn pane_content_rect(&self, id: PaneId) -> Option<Rect> {
        let panes = panes_area(self.last_area, self.side);
        let local = local_panes(panes);
        let rect = if let Some(zoomed) = self.zoomed_pane() {
            if zoomed != id {
                return None;
            }
            roster_core::Rect::new(0, 0, panes.width, panes.height)
        } else {
            self.session
                .layout(panes.width, panes.height)
                .into_iter()
                .find(|(pane, _)| *pane == id)?
                .1
        };
        let content_local = content_rect(rect, local);
        Some(Rect::new(
            panes.x + content_local.x,
            panes.y + content_local.y,
            content_local.width,
            content_local.height,
        ))
    }

    /// What sits under (`x`, `y`), with app-state refinements the pure
    /// hit-test can't see: an exited pane's content resolves to its
    /// overlay's restart/close buttons.
    fn hit_at(&self, x: u16, y: u16) -> Hit {
        let hit = hit_test(
            self.last_area,
            &self.session,
            self.side,
            &self.last_entries,
            self.zoomed_pane(),
            (x, y),
        );
        let Hit::Pane(id) = hit else {
            return hit;
        };
        if self.runtimes.get(&id).is_none_or(|rt| rt.exited.is_none()) {
            return hit;
        }
        let Some(content) = self.pane_content_rect(id) else {
            return hit;
        };
        if let Some((restart, close)) = exited_buttons(content) {
            let within = |r: Rect| x >= r.x && x < r.x + r.width && y == r.y;
            if within(restart) {
                return Hit::PaneRestart(id);
            }
            if within(close) {
                return Hit::PaneClose(id);
            }
        }
        hit
    }

    /// Relaunch an exited pane's command in place.
    fn restart_pane(&mut self, id: PaneId) {
        let Some(command) = self.session.pane(id).and_then(|p| p.command.clone()) else {
            return;
        };
        if let Some(remote) = &mut self.remote {
            remote.pending.push_back(Placement::Replace(id));
            remote.send(&Frame::Spawn { command });
            return;
        }
        if let Err(error) = self.attach(id, &command) {
            self.toast(format!("restart failed: {error}"), ToastLevel::Error);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // The launcher owns its input state; take it out so launching can
        // borrow the whole app, and put it back unless the mode ended.
        if matches!(self.mode, Mode::Launch(_)) {
            let Mode::Launch(mut state) = std::mem::replace(&mut self.mode, Mode::Normal) else {
                unreachable!("matched Launch above");
            };
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Some(command) = state.command(&self.launchables) {
                        self.launch(&command);
                    }
                }
                code => {
                    match code {
                        KeyCode::Down => state.select_next(&self.launchables),
                        KeyCode::Up => state.select_prev(&self.launchables),
                        KeyCode::Backspace => state.backspace(),
                        // Tab expands the selection into the input so its
                        // flags can be edited before launching.
                        KeyCode::Tab => state.expand(&self.launchables),
                        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.select_next(&self.launchables);
                        }
                        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.select_prev(&self.launchables);
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.push(c);
                        }
                        _ => {}
                    }
                    self.mode = Mode::Launch(state);
                }
            }
            return;
        }
        match self.mode {
            Mode::Normal => {
                if is_prefix(&key) {
                    self.mode = Mode::Prefix;
                } else if let Some(bytes) = encode_key(&key) {
                    self.write_to_focused(&bytes);
                }
            }
            Mode::Prefix => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('c') => self.mode = Mode::Launch(LauncherState::new()),
                    KeyCode::Char('%') => self.split(SplitDirection::Horizontal),
                    KeyCode::Char('"') => self.split(SplitDirection::Vertical),
                    KeyCode::Char('o') => self.session.focus_next(),
                    KeyCode::Char('n') => self.session.next_window(),
                    KeyCode::Char('p') => self.session.prev_window(),
                    KeyCode::Char('z') => self.zoomed = !self.zoomed,
                    KeyCode::Char('x') => {
                        if let Some(id) = self.session.focused() {
                            self.request_close(id);
                        }
                    }
                    KeyCode::Char('j') => {
                        self.sidebar = SidebarState::anchored(&self.last_entries);
                        self.mode = Mode::Jump;
                    }
                    KeyCode::Char('d') => self.detach(),
                    // Quitting a persistent session detaches — leaving must
                    // never silently kill the agents.
                    KeyCode::Char('q') => {
                        if self.remote.is_some() {
                            self.detach();
                        } else {
                            self.quit = true;
                        }
                    }
                    KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.write_to_focused(&[0x02]);
                    }
                    _ => {}
                }
            }
            Mode::ConfirmClose(id) => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => self.close_pane(id),
                    _ => {} // anything else cancels
                }
            }
            Mode::Jump => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.sidebar.select_next(&self.last_entries);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sidebar.select_prev(&self.last_entries);
                }
                // Toggle auto-approve on the selected agent and stay in jump
                // mode. Forward-looking: it answers the pane's *next* asks,
                // not any prompt already waiting.
                KeyCode::Char('a') => {
                    if let Some(index) = self.sidebar.selected(&self.last_entries) {
                        let pane = self.last_entries[index].pane.raw();
                        self.toggle_auto_approve(pane);
                    }
                }
                KeyCode::Enter => {
                    if let Some(Message::JumpToPane(id)) = self.sidebar.activate(&self.last_entries)
                    {
                        self.session.focus(id);
                    }
                    self.mode = Mode::Normal;
                }
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::Launch(_) => unreachable!("handled above"),
        }
    }

    /// Translate mouse input into the same intents keys produce: click a
    /// sidebar card to jump, a pane to focus it, the pinned `+ new agent`
    /// button to open the launcher, a title's `✕` to close its pane, a
    /// launcher row to launch; drag a divider to resize; scroll to nudge
    /// the pane under the cursor.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let (x, y) = (mouse.column, mouse.row);
        self.last_mouse = Some((x, y));

        // The confirm dialog owns the mouse while open: its buttons decide,
        // and clicking anywhere else dismisses it.
        if let Mode::ConfirmClose(pending) = self.mode {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    match confirm_button_at(self.last_area, x, y) {
                        Some(ConfirmButton::Close) => {
                            self.mode = Mode::Normal;
                            self.close_pane(pending);
                        }
                        Some(ConfirmButton::Cancel) => self.mode = Mode::Normal,
                        None => {
                            if !confirm_contains(self.last_area, x, y) {
                                self.mode = Mode::Normal;
                            }
                        }
                    }
                }
                MouseEventKind::Moved => {
                    set_pointer(
                        &mut self.pointer,
                        if confirm_button_at(self.last_area, x, y).is_some() {
                            Pointer::Hand
                        } else {
                            Pointer::Default
                        },
                    );
                }
                _ => {}
            }
            return;
        }

        // The launcher owns the mouse while open: hovering a row selects
        // it, clicking launches it.
        let welcome = self.placeholder.is_some();
        let launch_area = self.last_area;
        if let Mode::Launch(state) = &mut self.mode {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let launcher = Launcher::new(&self.launchables, state).welcome(welcome);
                    if let Some(index) = launcher.item_at(launch_area, x, y) {
                        state.select(index);
                        let command = state.command(&self.launchables);
                        self.mode = Mode::Normal;
                        if let Some(command) = command {
                            self.launch(&command);
                        }
                    } else if !launcher.contains(launch_area, x, y) {
                        self.mode = Mode::Normal;
                    }
                }
                MouseEventKind::Moved => {
                    let launcher = Launcher::new(&self.launchables, state).welcome(welcome);
                    let item = launcher.item_at(launch_area, x, y);
                    if let Some(index) = item {
                        state.select(index);
                    }
                    set_pointer(
                        &mut self.pointer,
                        if item.is_some() {
                            Pointer::Hand
                        } else {
                            Pointer::Default
                        },
                    );
                }
                MouseEventKind::ScrollDown => state.select_next(&self.launchables),
                MouseEventKind::ScrollUp => state.select_prev(&self.launchables),
                _ => {}
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Toasts sit above everything else: a click on one
                // dismisses it and goes no further.
                if let Some(index) = self.toast_at(x, y) {
                    self.toasts.remove(index);
                    return;
                }
                // Any click drops the previous selection, like a terminal.
                self.selection = None;
                let hit = self.hit_at(x, y);
                match hit {
                    Hit::SidebarEntry(index) => {
                        if let Some(entry) = self.last_entries.get(index) {
                            self.session.focus(entry.pane);
                        }
                    }
                    Hit::SidebarAuto(index) => {
                        // The chip is a button on the card, not the card:
                        // toggle without stealing focus from the pane the
                        // user is watching.
                        if let Some(pane) = self.last_entries.get(index).map(|e| e.pane.raw()) {
                            self.toggle_auto_approve(pane);
                        }
                    }
                    Hit::SidebarAutoAll => self.toggle_auto_all(),
                    Hit::SidebarNewAgent => {
                        self.mode = Mode::Launch(LauncherState::new());
                    }
                    Hit::SidebarViewGrid => self.zoomed = false,
                    Hit::SidebarViewSolo => self.zoomed = true,
                    Hit::StatusWindows => self.session.next_window(),
                    Hit::PaneClose(id) => self.request_close(id),
                    Hit::PaneRestart(id) => self.restart_pane(id),
                    Hit::PaneTitle(id) | Hit::Pane(id) => {
                        self.session.focus(id);
                        // Double-clicking a title toggles solo, like
                        // double-clicking a window's title bar maximizes.
                        let double = self.last_click.is_some_and(|(at, pos)| {
                            at.elapsed() < DOUBLE_CLICK_WINDOW && pos == (x, y)
                        });
                        if double && matches!(hit, Hit::PaneTitle(_)) {
                            self.zoomed = !self.zoomed;
                        }
                        // Title rows and separator columns double as split
                        // dividers; grab one if it's there. Solo view has
                        // no dividers.
                        let panes = panes_area(self.last_area, self.side);
                        let mut grabbed = false;
                        if !self.zoomed && x >= panes.x && y >= panes.y {
                            let local = (x - panes.x, y - panes.y);
                            if self
                                .session
                                .divider_at(panes.width, panes.height, local.0, local.1)
                                .is_some()
                            {
                                self.dragging = Some(local);
                                grabbed = true;
                            }
                        }
                        // Pressing on live content anchors a text
                        // selection; it only becomes one if the mouse
                        // moves before release.
                        if !grabbed && matches!(hit, Hit::Pane(_)) {
                            if let Some(content) = self.pane_content_rect(id) {
                                if x >= content.x
                                    && x < content.x + content.width
                                    && y >= content.y
                                    && y < content.y + content.height
                                {
                                    self.sel_anchor = Some((id, (x - content.x, y - content.y)));
                                }
                            }
                        }
                    }
                    Hit::Sidebar | Hit::Status | Hit::Outside => {}
                }
                self.last_click = Some((Instant::now(), (x, y)));
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.dragging {
                    let panes = panes_area(self.last_area, self.side);
                    let to = (x.saturating_sub(panes.x), y.saturating_sub(panes.y));
                    if let Some(new_pos) =
                        self.session
                            .drag_divider(panes.width, panes.height, from, to)
                    {
                        self.dragging = Some(new_pos);
                    }
                } else if let Some((id, anchor)) = self.sel_anchor {
                    // Extend the selection to the cell under the pointer,
                    // clamped into the pane's content.
                    if let Some(content) = self.pane_content_rect(id) {
                        let clamp = |v: u16, lo: u16, hi: u16| v.max(lo).min(hi);
                        let cx = clamp(x, content.x, content.x + content.width.saturating_sub(1));
                        let cy = clamp(y, content.y, content.y + content.height.saturating_sub(1));
                        self.selection = Some((id, anchor, (cx - content.x, cy - content.y)));
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = None;
                self.sel_anchor = None;
                // A completed drag-selection copies itself — click-drag,
                // release, pasted anywhere.
                if let Some((id, a, b)) = self.selection {
                    let text = self
                        .runtimes
                        .get(&id)
                        .map(|rt| {
                            rt.screen.grid().linear_text(
                                (usize::from(a.0), usize::from(a.1)),
                                (usize::from(b.0), usize::from(b.1)),
                            )
                        })
                        .unwrap_or_default();
                    if text.trim().is_empty() {
                        self.selection = None;
                    } else {
                        copy_to_clipboard(&text);
                        let lines = text.lines().count();
                        self.toast(
                            if lines > 1 {
                                format!("copied {lines} lines")
                            } else {
                                "copied".to_string()
                            },
                            ToastLevel::Info,
                        );
                    }
                }
            }
            MouseEventKind::Moved => {
                let shape = self.pointer_shape_at(x, y);
                set_pointer(&mut self.pointer, shape);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let hit = self.hit_at(x, y);
                if let Hit::Pane(id)
                | Hit::PaneTitle(id)
                | Hit::PaneClose(id)
                | Hit::PaneRestart(id) = hit
                {
                    let up = mouse.kind == MouseEventKind::ScrollUp;
                    // Pane-local, 1-based coords for a forwarded mouse report;
                    // computed before the mutable runtime borrow.
                    let local = self.pane_content_rect(id).map(|c| {
                        let col = x.saturating_sub(c.x).min(c.width.saturating_sub(1)) + 1;
                        let row = y.saturating_sub(c.y).min(c.height.saturating_sub(1)) + 1;
                        (col, row)
                    });
                    if let Some(rt) = self.runtimes.get_mut(&id) {
                        // A dead child can't receive input, so wheel_action
                        // routes exited panes to a history scroll instead.
                        match wheel_action(
                            up,
                            rt.screen.mouse_reporting(),
                            rt.screen.sgr_mouse(),
                            rt.screen.alternate_screen(),
                            rt.exited.is_none(),
                            local,
                        ) {
                            Some(WheelAction::Forward(bytes)) => {
                                let _ = rt.io.write(&bytes);
                            }
                            Some(WheelAction::Scroll(delta)) => rt.screen.scroll_display(delta),
                            None => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The pointer shape for the position: a hand over anything clickable,
    /// resize arrows over a draggable divider, the plain arrow everywhere
    /// else. Buttons win over dividers — a ✕ that shares a divider row
    /// still reads as a button.
    fn pointer_shape_at(&self, x: u16, y: u16) -> Pointer {
        if self.toast_at(x, y).is_some() {
            return Pointer::Hand;
        }
        let hit = self.hit_at(x, y);
        if matches!(
            hit,
            Hit::PaneClose(_)
                | Hit::PaneRestart(_)
                | Hit::SidebarNewAgent
                | Hit::SidebarViewGrid
                | Hit::SidebarViewSolo
                | Hit::SidebarEntry(_)
                | Hit::SidebarAuto(_)
                | Hit::SidebarAutoAll
                | Hit::StatusWindows
        ) {
            return Pointer::Hand;
        }
        let panes = panes_area(self.last_area, self.side);
        if !self.zoomed
            && x >= panes.x
            && y >= panes.y
            && x < panes.x + panes.width
            && y < panes.y + panes.height
        {
            if let Some(direction) =
                self.session
                    .divider_at(panes.width, panes.height, x - panes.x, y - panes.y)
            {
                return match direction {
                    SplitDirection::Horizontal => Pointer::ResizeEw,
                    SplitDirection::Vertical => Pointer::ResizeNs,
                };
            }
        }
        pointer_for(hit)
    }

    /// Start `command` in its own fresh window. The bare-start backdrop
    /// shell, if one exists, is replaced by this first launch regardless of
    /// focus or whether the user typed into it — a plain shell never
    /// survives as its own workspace.
    fn launch(&mut self, command: &str) {
        // In a session the server spawns; the pane lands when PaneOpened
        // comes back, replacing the backdrop placeholder if there is one.
        if self.remote.is_some() {
            let placement = match self.placeholder {
                Some(id) => Placement::Replace(id),
                None => Placement::Window,
            };
            if let Some(remote) = &mut self.remote {
                remote.pending.push_back(placement);
                remote.send(&Frame::Spawn {
                    command: command.to_string(),
                });
            }
            return;
        }
        if let Some(id) = self.placeholder.take() {
            // Spawn first: a failed launch keeps the shell running.
            let old = self.runtimes.remove(&id);
            match self.attach(id, command) {
                Ok(()) => {} // dropping the old runtime kills the shell
                Err(error) => {
                    if let Some(rt) = old {
                        self.runtimes.insert(id, rt);
                    }
                    self.toast(format!("launch failed: {error}"), ToastLevel::Error);
                }
            }
            return;
        }
        let id = self.session.new_window();
        if let Err(error) = self.attach(id, command) {
            self.session.close(id);
            self.toast(format!("launch failed: {error}"), ToastLevel::Error);
        }
    }

    /// Split the focused pane and run a fresh shell in the new half.
    fn split(&mut self, direction: SplitDirection) {
        let Some(target) = self.session.focused() else {
            return;
        };
        if let Some(remote) = &mut self.remote {
            remote
                .pending
                .push_back(Placement::Split(target, direction));
            remote.send(&Frame::Spawn {
                command: default_shell(),
            });
            return;
        }
        let Some(id) = self.session.split(target, direction) else {
            return;
        };
        let shell = default_shell();
        if self.attach(id, &shell).is_err() {
            self.session.close(id);
        }
    }

    /// A pane's display name: its agent card's name when detected, else its
    /// command.
    fn pane_name(&self, id: PaneId) -> String {
        self.last_entries
            .iter()
            .find(|e| e.pane == id)
            .map(|e| e.agent.clone())
            .or_else(|| self.session.pane(id).and_then(|p| p.command.clone()))
            .unwrap_or_else(|| "agent".to_string())
    }

    /// Close `id`, but ask first when it would kill a live agent — a stray
    /// click on ✕ shouldn't take a working agent down. Shells and exited
    /// panes close immediately.
    fn request_close(&mut self, id: PaneId) {
        let live_agent = self
            .runtimes
            .get(&id)
            .is_some_and(|rt| rt.kind.is_some() && rt.exited.is_none());
        if live_agent {
            self.mode = Mode::ConfirmClose(id);
        } else {
            self.close_pane(id);
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        if self.placeholder == Some(id) {
            self.placeholder = None;
        }
        if self.selection.map(|s| s.0) == Some(id) {
            self.selection = None;
        }
        if self.sel_anchor.map(|s| s.0) == Some(id) {
            self.sel_anchor = None;
        }
        // A session-owned pane dies on the server, not by drop.
        if let Some(PaneIo::Remote { pane, .. }) = self.runtimes.get(&id).map(|rt| &rt.io) {
            self.remote_send(&Frame::Close { pane: *pane });
        }
        // Dropping the runtime kills and reaps the child.
        self.runtimes.remove(&id);
        self.session.close(id);
    }

    /// Deliver a host-terminal paste. In the launcher it feeds the filter
    /// input; otherwise it goes to the focused pane — wrapped in paste
    /// guards when the application asked for bracketed paste (so multi-line
    /// prompts arrive as one paste, not keystrokes), with newlines
    /// normalized to carriage returns when it didn't (what a terminal's
    /// enter key sends).
    fn handle_paste(&mut self, text: &str) {
        if let Mode::Launch(state) = &mut self.mode {
            for c in text.chars().filter(|c| !c.is_control()) {
                state.push(c);
            }
            return;
        }
        let Some(id) = self.session.focused() else {
            return;
        };
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.exited.is_some() {
            return;
        }
        rt.screen.scroll_to_bottom();
        let _ = if rt.screen.bracketed_paste() {
            // Strip any end guard the clipboard could smuggle in: text must
            // not be able to fake the end of the paste and inject input.
            let safe = text.replace("\x1b[201~", "");
            let mut framed = Vec::with_capacity(safe.len() + 12);
            framed.extend_from_slice(b"\x1b[200~");
            framed.extend_from_slice(safe.as_bytes());
            framed.extend_from_slice(b"\x1b[201~");
            rt.io.write(&framed)
        } else {
            let safe = text.replace("\r\n", "\r").replace('\n', "\r");
            rt.io.write(safe.as_bytes())
        };
    }

    fn write_to_focused(&mut self, bytes: &[u8]) {
        let Some(id) = self.session.focused() else {
            return;
        };
        // Typing drops the selection and snaps back to live output.
        self.selection = None;
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.exited.is_some() {
            return;
        }
        rt.screen.scroll_to_bottom();
        if rt.io.write(bytes).is_err() {
            self.mark_exited(id);
        }
    }

    /// The status line: a badge while a mode is armed, plus contextual key
    /// hints.
    fn status_line(&self) -> (Option<&'static str>, String) {
        match &self.mode {
            Mode::Normal => {
                // "focused ▸ claude" says where keystrokes go; a pane with
                // no known command drops the prefix rather than dangling.
                let focused = self
                    .session
                    .focused()
                    .and_then(|id| self.session.pane(id))
                    .and_then(|pane| pane.command.as_deref())
                    .map(|command| format!("focused ▸ {command} · "))
                    .unwrap_or_default();
                if self.zoomed {
                    (
                        Some("SOLO"),
                        format!(
                            "{focused}click a card to switch · ctrl-b: keys · then z: grid · j: jump · q: quit roster"
                        ),
                    )
                } else {
                    (
                        None,
                        format!(
                            "{focused}ctrl-b: keys · then c: new agent · j: jump · z: solo · x: close agent · d: detach · q: quit roster"
                        ),
                    )
                }
            }
            Mode::Prefix => (
                Some("PREFIX"),
                "c: new agent · n/p: windows · z: solo · %/\": split · o: focus · j: jump · x: close agent · d: detach · q: quit roster"
                    .to_string(),
            ),
            Mode::Jump => (
                Some("JUMP"),
                "j/k: move · enter: jump to pane · a: auto-approve · esc: cancel".to_string(),
            ),
            Mode::Launch(_) => (
                Some("LAUNCH"),
                "type to filter or run a command · enter: launch · tab: edit flags · esc: cancel"
                    .to_string(),
            ),
            Mode::ConfirmClose(_) => (
                Some("CLOSE?"),
                "y/enter: close · esc: cancel".to_string(),
            ),
        }
    }
}

fn is_prefix(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// What a wheel notch does over a pane, decided by the guest's terminal
/// modes.
enum WheelAction {
    /// Bytes to write to the guest PTY — a mouse report or arrow keys.
    Forward(Vec<u8>),
    /// Move roster's own scrollback by this many lines (positive = up).
    Scroll(i32),
}

/// Route a wheel notch by what the guest negotiated. A live app that tracks
/// the mouse *and* speaks SGR (Claude Code turns on DECSET 1000/1002/1003 +
/// 1006) handles the wheel itself — forward a real mouse report so it
/// scrolls its own transcript, rather than arrow keys it would read as
/// cursor/selection movement. A live full-screen app that does not — a
/// pager, or a legacy-mouse app roster can't encode for — gets arrows, the
/// closest thing to scroll it understands. Everything else, including any
/// exited pane whose child can no longer receive input, moves roster's own
/// scrollback. `pos` is the pane-local 1-based pointer; a forwarding guest
/// with no resolvable position gets nothing.
fn wheel_action(
    up: bool,
    mouse_reporting: bool,
    sgr_mouse: bool,
    alt_screen: bool,
    alive: bool,
    pos: Option<(u16, u16)>,
) -> Option<WheelAction> {
    if alive && mouse_reporting && sgr_mouse {
        pos.map(|(col, row)| WheelAction::Forward(sgr_wheel(up, col, row)))
    } else if alive && alt_screen {
        let arrows: &[u8] = if up {
            b"\x1b[A\x1b[A\x1b[A"
        } else {
            b"\x1b[B\x1b[B\x1b[B"
        };
        Some(WheelAction::Forward(arrows.to_vec()))
    } else {
        Some(WheelAction::Scroll(if up { 3 } else { -3 }))
    }
}

/// Encode a wheel notch as an SGR mouse report (DECSET 1006) at 1-based
/// `(col, row)`. Wheel up is button 64, wheel down 65; wheel events are
/// press-only, so the sequence always ends in `M`. Callers must confirm the
/// guest negotiated SGR first — a legacy-encoding guest would misread these
/// bytes.
fn sgr_wheel(up: bool, col: u16, row: u16) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    format!("\x1b[<{button};{col};{row}M").into_bytes()
}

/// Hand the text to the hosting terminal's clipboard via OSC 52 — it works
/// locally and through SSH alike, wherever the terminal supports it.
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

/// Standard base64 with padding — small enough to not warrant a dependency.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Tell the terminal which pointer to show (OSC 22, the xterm pointer-shape
/// protocol), skipping the write when the shape hasn't changed. Terminals
/// without support ignore the sequence.
fn set_pointer(current: &mut Pointer, shape: Pointer) {
    use std::io::Write;
    if *current == shape {
        return;
    }
    *current = shape;
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]22;{}\x07", shape.name());
    let _ = out.flush();
}

/// The user's shell, for panes roster opens itself.
pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Listen for `roster _hook` and `roster _statusline` reports on a
/// per-process unix socket, feeding frames into the app loop. Returns the
/// path to advertise via `ROSTER_HOOK_SOCK`, or `None` when the socket
/// can't be created — hook state and telemetry are an upgrade over
/// scraping, never a requirement.
///
/// The socket lives in a `hook/` subdirectory, NOT next to the session
/// sockets: `roster ls`/`kill`/`attach` probe every top-level `*.sock` as
/// a session, and this listener speaks no session protocol.
///
/// `auto_approve` is the shared set of auto-approved panes: on a `HookBlocked`
/// (a permission ask) the reader answers the hook with a [`Frame::HookDecision`]
/// on the same connection — `allow` iff the pane is in the set — *before*
/// forwarding the frame to the app loop, so a busy loop can't delay the
/// decision. Every ask gets a reply (the common `allow: false` too), so the
/// hook never waits out its deadline on the normal path. A poisoned lock
/// degrades to `allow: false` and keeps the listener alive.
fn start_hook_listener(
    tx: Sender<Output>,
    auto_approve: Arc<Mutex<HashSet<u64>>>,
) -> Option<PathBuf> {
    let base = crate::server::vetted_sessions_dir().ok()?;
    let dir = base.join("hook");
    crate::server::ensure_private_dir(&dir).ok()?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    // A stale socket from a recycled pid would block the bind. Unlike the
    // session dir, nothing long-lived legitimately owns this name.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                // Persistent accept errors (fd exhaustion) must degrade,
                // not busy-spin a core.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            let tx = tx.clone();
            let auto_approve = Arc::clone(&auto_approve);
            // One bounded thread per hook invocation: a real `_hook` sends
            // one frame, reads its reply (asks only), and disconnects. The
            // read timeout and frame cap keep a stray or hostile peer from
            // parking threads forever.
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                for _ in 0..16 {
                    match read_frame(&mut stream) {
                        Ok(Some(Frame::HookBlocked { pane, tool, reason })) => {
                            // Answer the ask on this same connection before
                            // handing it to the app loop. A poisoned lock is
                            // a safe `false` (ask the human), never a panic.
                            let allow = auto_approve
                                .lock()
                                .map(|set| set.contains(&pane))
                                .unwrap_or(false);
                            let _ = write_frame(&mut stream, &Frame::HookDecision { allow });
                            let frame = Frame::HookBlocked { pane, tool, reason };
                            if tx.send(Output::Hook(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(Some(frame @ (Frame::HookClear { .. } | Frame::Statusline { .. }))) => {
                            if tx.send(Output::Hook(frame)).is_err() {
                                return;
                            }
                        }
                        // Anything else — other frame types, timeouts,
                        // corruption, EOF — ends the connection: this
                        // socket speaks hook frames only.
                        _ => return,
                    }
                }
            });
        }
    });
    Some(path)
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(path) = &self.hook_sock {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(
            base64("selected text ✓".as_bytes()),
            "c2VsZWN0ZWQgdGV4dCDinJM="
        );
    }

    #[test]
    fn sgr_wheel_encodes_button_and_position() {
        // Wheel up is button 64, down 65; SGR is 1-based and press-only (M).
        assert_eq!(sgr_wheel(true, 1, 1), b"\x1b[<64;1;1M");
        assert_eq!(sgr_wheel(false, 12, 7), b"\x1b[<65;12;7M");
    }

    #[test]
    fn wheel_routes_by_guest_mode() {
        let pos = Some((5, 8));
        // args: up, mouse_reporting, sgr_mouse, alt_screen, alive, pos.
        // A live SGR mouse-tracking guest (Claude Code) gets a forwarded
        // report, NOT arrow keys — the regression that motivated the change.
        assert!(matches!(
            wheel_action(true, true, true, true, true, pos),
            Some(WheelAction::Forward(b)) if b == sgr_wheel(true, 5, 8)
        ));
        // A tracking guest that did NOT negotiate SGR (legacy X10 encoding)
        // must not be sent SGR bytes; it falls back to the arrows path.
        assert!(matches!(
            wheel_action(true, true, false, true, true, pos),
            Some(WheelAction::Forward(b)) if b == b"\x1b[A\x1b[A\x1b[A"
        ));
        // A live full-screen app without mouse tracking (a pager) gets arrows.
        assert!(matches!(
            wheel_action(true, false, false, true, true, pos),
            Some(WheelAction::Forward(b)) if b == b"\x1b[A\x1b[A\x1b[A"
        ));
        assert!(matches!(
            wheel_action(false, false, false, true, true, pos),
            Some(WheelAction::Forward(b)) if b == b"\x1b[B\x1b[B\x1b[B"
        ));
        // Plain output scrolls roster's own history; up is positive.
        assert!(matches!(
            wheel_action(true, false, false, false, true, pos),
            Some(WheelAction::Scroll(3))
        ));
        assert!(matches!(
            wheel_action(false, false, false, false, true, pos),
            Some(WheelAction::Scroll(-3))
        ));
        // An exited pane can't receive input; even a tracking+SGR guest
        // routes to a history scroll rather than a dropped forward.
        assert!(matches!(
            wheel_action(true, true, true, true, false, pos),
            Some(WheelAction::Scroll(3))
        ));
        // A live forwarding guest with no resolvable pointer does nothing.
        assert!(wheel_action(true, true, true, true, true, None).is_none());
    }

    #[test]
    fn a_new_agent_inherits_auto_yes_only_when_the_whole_fleet_is_armed() {
        // No cards: the header toggle isn't lit, so the first agent spawned
        // into an empty fleet never inherits (reattach also hits this — it
        // attaches panes before last_entries is ever computed).
        assert!(!fleet_auto_armed(&[]));
        // One un-armed agent leaves the toggle off; a newcomer stays opt-in.
        assert!(!fleet_auto_armed(&[false]));
        assert!(!fleet_auto_armed(&[true, false, true]));
        // Every existing agent armed lights the toggle — the newcomer joins
        // armed so creating it doesn't silently un-light the fleet.
        assert!(fleet_auto_armed(&[true]));
        assert!(fleet_auto_armed(&[true, true, true]));
    }

    #[test]
    fn hook_still_bypasses_debounce() {
        // A fresh pin outranks the committed scrape no matter what state
        // the debouncer settled on — the hook event was never fed through
        // it, and a blocked pin needs no consecutive-reading cushion.
        for scraped in [
            AgentState::Idle,
            AgentState::Working,
            AgentState::Done,
            AgentState::Blocked,
        ] {
            assert!(
                hook_pin_wins(Duration::ZERO, scraped),
                "a fresh pin must outrank a {scraped:?} scrape"
            );
        }
        // Past the paint grace the settled screen wins on reality — except
        // when it still reads blocked, where the pin's verbatim ask stays.
        assert!(!hook_pin_wins(HOOK_PIN_GRACE, AgentState::Idle));
        assert!(!hook_pin_wins(HOOK_PIN_GRACE, AgentState::Working));
        assert!(hook_pin_wins(HOOK_PIN_GRACE, AgentState::Blocked));
    }

    /// `start_hook_listener` binds a pid-keyed socket path and the test
    /// binary is one pid: two listener tests on parallel threads would
    /// unlink and steal each other's socket. Serialize them.
    static LISTENER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn hook_listener_answers_asks_from_the_auto_approve_set() {
        use std::os::unix::net::UnixStream;

        let _serial = LISTENER_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (tx, rx) = mpsc::channel::<Output>();
        let auto: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        auto.lock().unwrap().insert(7);
        let Some(path) = start_hook_listener(tx, Arc::clone(&auto)) else {
            // No sessions dir available (locked-down sandbox): nothing to do.
            return;
        };

        // An auto-approved pane's ask is answered allow: true, and the block
        // still reaches the app loop for the card.
        let mut s = UnixStream::connect(&path).expect("connect hook socket");
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write_frame(
            &mut s,
            &Frame::HookBlocked {
                pane: 7,
                tool: "Bash".into(),
                reason: "Bash: ls".into(),
            },
        )
        .unwrap();
        assert_eq!(
            read_frame(&mut s).unwrap(),
            Some(Frame::HookDecision { allow: true }),
            "auto-approved pane must be allowed"
        );
        assert!(
            matches!(
                rx.recv_timeout(Duration::from_secs(2)),
                Ok(Output::Hook(Frame::HookBlocked { pane: 7, .. }))
            ),
            "the ask must still reach the app loop"
        );
        drop(s);

        // A pane that isn't in the set is answered allow: false — Claude asks
        // the human, exactly as before.
        let mut s = UnixStream::connect(&path).expect("connect hook socket");
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write_frame(
            &mut s,
            &Frame::HookBlocked {
                pane: 9,
                tool: String::new(),
                reason: "x".into(),
            },
        )
        .unwrap();
        assert_eq!(
            read_frame(&mut s).unwrap(),
            Some(Frame::HookDecision { allow: false }),
            "a non-auto pane must not be auto-approved"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hook_listener_forwards_statusline_frames_without_a_reply() {
        use std::os::unix::net::UnixStream;

        let _serial = LISTENER_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (tx, rx) = mpsc::channel::<Output>();
        let auto: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let Some(path) = start_hook_listener(tx, auto) else {
            // No sessions dir available (locked-down sandbox): nothing to do.
            return;
        };

        let mut s = UnixStream::connect(&path).expect("connect hook socket");
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let json = r#"{"model":{"display_name":"Opus"}}"#.to_string();
        write_frame(
            &mut s,
            &Frame::Statusline {
                pane: 4,
                json: json.clone(),
            },
        )
        .unwrap();
        let Ok(Output::Hook(Frame::Statusline { pane, json: seen })) =
            rx.recv_timeout(Duration::from_secs(2))
        else {
            panic!("the statusline frame never reached the app loop");
        };
        assert_eq!(pane, 4);
        assert_eq!(seen, json, "the payload must arrive verbatim");
        // Telemetry is fire-and-forget: nothing comes back — the read either
        // times out (the listener kept the connection for more frames) or
        // sees a clean close, never a reply frame.
        match read_frame(&mut s) {
            Ok(None) | Err(_) => {}
            Ok(Some(frame)) => panic!("unexpected reply to a statusline report: {frame:?}"),
        }

        let _ = std::fs::remove_file(&path);
    }
}
