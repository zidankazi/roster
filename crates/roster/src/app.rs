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
use std::time::{Duration, Instant, SystemTime};

use roster_proto::{read_frame, write_frame, Frame};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use roster_core::{
    carry_rate_limit, fleet_rate_limit, AgentState, ContextAlert, LimitNotifier, PaneId, RateLimit,
    Session, SplitDirection,
};
use roster_detect::{AgentKind, Detector, PaneTracker};
use roster_pty::Pty;
use roster_term::Screen;
use roster_tui::{
    confirm_button_at, confirm_contains, content_rect, exited_buttons, hit_test, launch_items,
    menu_contains, menu_fits, menu_item_at, panes_area, pin_to_top, pointer_for, render,
    shell_entries, sidebar_entries, toast_rects, ConfirmButton, ContextMenuItem, ContextMenuView,
    Hit, HitContext, LaunchItem, Launcher, LauncherState, Message, Pointer, ShellEntry,
    SidebarEntry, SidebarSide, SidebarState, ToastLevel, View,
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

/// The launch directory for the sidebar's workspace row, tilde-collapsed
/// against `$HOME` when it sits underneath it (`~/Desktop/roster`, or `~`
/// exactly at home). `None` when the process's cwd can't be read — the
/// sidebar simply drops its title rows rather than showing a stale guess.
fn current_workspace() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Some(cwd.display().to_string());
    };
    if cwd == home {
        return Some("~".to_string());
    }
    match cwd.strip_prefix(&home) {
        Ok(rest) => Some(format!("~/{}", rest.display())),
        Err(_) => Some(cwd.display().to_string()),
    }
}

/// The wall clock, local time, formatted `HH:MM` for the sidebar's
/// workspace row. Hand-rolled via `libc::localtime_r` — already an unsafe
/// dependency of this binary (see `server.rs`) — rather than pulling in a
/// timezone crate for one conversion.
fn local_clock(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs, &mut tm);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}
/// One detection tick of the account rate-limit carry: the new displayed
/// reading and its stamps, from the freshest live aggregation and the
/// previously displayed reading (`roster_core::carry_rate_limit`). With no
/// identified agent pane left there is nothing to carry *for* — the footer
/// clears rather than asserting limits over an agentless session. Elapsed
/// takes the larger of the two clocks (see `rate_limits_at`), and the
/// carried value is re-stamped, so aging accumulates tick over tick. Live
/// readings display as they arrived — the carry only ages what feeds have
/// gone quiet on — so a countdown can run up to the tracker's 30s ageout
/// behind reality before the carry takes over; the same freshness slack
/// every badge already has.
fn carry_tick(
    live: Option<RateLimit>,
    held: (Option<RateLimit>, Option<(Instant, SystemTime)>),
    agents_present: bool,
    now: Instant,
    wall: SystemTime,
) -> (Option<RateLimit>, Option<(Instant, SystemTime)>) {
    if !agents_present {
        return (None, None);
    }
    let (held_limits, held_at) = held;
    let elapsed = held_at
        .map(|(mono, wall_then)| {
            now.saturating_duration_since(mono)
                .max(wall.duration_since(wall_then).unwrap_or(Duration::ZERO))
        })
        .unwrap_or(Duration::ZERO);
    let limits = carry_rate_limit(live, held_limits.as_ref(), elapsed);
    let stamps = limits.is_some().then_some((now, wall));
    (limits, stamps)
}
/// Whether the sidebar header's `auto-yes` fleet toggle reads armed for
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
    /// A sidebar card's right-click menu is open for `pane`, drawn at
    /// `anchor` (the clicked cell); it owns the mouse until an item or an
    /// outside click closes it.
    ContextMenu { pane: PaneId, anchor: (u16, u16) },
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

/// A hook-reported tool call supplying a working pane's reason — the current
/// activity (`Bash: cargo test`) in place of the scraped spinner. Unlike a
/// [`HookPin`] it never changes the *state*: it is shown only while the
/// screen already reads working, so the screen owns reality and the hook
/// owns the richer wording.
struct ActivityPin {
    /// The verbatim activity, shown as the working card's reason.
    reason: String,
    /// When it landed: refreshed on each `PreToolUse`, and its age guards
    /// the clear against a screen frame that hasn't painted the tool yet.
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
    /// A hook-reported current tool call. While set and the screen reads
    /// working, it replaces the scraped spinner as the card's reason.
    /// Refreshed on each `PreToolUse`, cleared on `Stop` and when a settled
    /// screen leaves working (see [`ActivityPin`]).
    hook_activity: Option<ActivityPin>,
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
    /// The sidebar's shells-section rows, rebuilt alongside `last_entries`
    /// each frame — but never fed into it: shells carry no state, so
    /// auto-approve inheritance, pinning, and triage never see them. The
    /// bare-start placeholder pane is filtered out here (it's scenery, not
    /// a shells-section tenant — see `placeholder`).
    last_shells: Vec<ShellEntry>,
    last_detect: Instant,
    /// The frame area of the most recent draw, for mouse hit-testing.
    last_area: Rect,
    /// A grabbed split divider, in pane-local coordinates, while dragging.
    dragging: Option<(u16, u16)>,
    /// A sidebar card pressed and possibly being dragged: the pane it names
    /// and the press cell. A release over a pane moves it in beside that pane
    /// (side-by-side view); a release still on the sidebar is a plain click
    /// that focuses it. Focus is deferred to the release so a drag doesn't
    /// swap the view out from under the drop target.
    card_drag: Option<(PaneId, (u16, u16))>,
    /// The bare-start shell pane: a backdrop for the launcher only. An
    /// ordinary shell (the launcher's `shell` row, a split) is a supported
    /// tenant and does survive as its own workspace — but this placeholder
    /// never does: the first launch replaces it unconditionally, and it is
    /// filtered out of the shells section rather than ever appearing as a
    /// row (see `last_shells`).
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
    /// A mouse-drag selection being made, `None` outside a drag — one
    /// field, so every path that ends the drag drops all of its state at
    /// once.
    sel_drag: Option<SelectionDrag>,
    /// The pane holding a left-button grab whose mouse events forward to
    /// the guest — a guest that negotiated SGR mouse tracking (Claude Code)
    /// gets the real mouse and runs its own drag-selection; roster's
    /// selection only serves panes that never asked.
    mouse_fwd: Option<PaneId>,
    /// The current selection: pane and both endpoints as content-local
    /// columns and scrollback-absolute rows (see [`absolute_row`]) —
    /// anchored to the text, not the screen. Highlighted (converted to
    /// viewport cells each frame) until the next click or keystroke;
    /// copied on release.
    selection: Option<(PaneId, SelPoint, SelPoint)>,
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
    /// The account's fleet-aggregated rate-limit reading, recomputed each
    /// detection tick from the panes' stamped telemetry (rate limits are
    /// account-scoped; the freshest live reading wins) and carried across
    /// quiet feeds (`roster_core::carry_rate_limit`) so idle fleets keep
    /// their footer. Drives the sidebar footer and the hit-test mirror;
    /// `None` — no reading live or carried — leaves both exactly as they
    /// were before the field existed.
    rate_limits: Option<RateLimit>,
    /// When `rate_limits` was last computed, on both clocks: the monotonic
    /// stamp keeps a backward clock step from freezing or rewinding the
    /// carry, the wall stamp keeps ticking through a system sleep (which
    /// pauses `Instant` on macOS and Linux), and the carry ages by
    /// whichever elapsed more — so a reset slept through still retires its
    /// window, and a forward clock step at worst retires a window early,
    /// absence being the honest failure mode. `None` exactly while
    /// `rate_limits` is `None`.
    rate_limits_at: Option<(Instant, SystemTime)>,
    /// Edge state behind the limit toasts: one notice per threshold per
    /// window, re-armed when usage falls back (see
    /// `roster_core::LimitNotifier`).
    limit_notifier: LimitNotifier,
    /// The launch directory, tilde-collapsed against `$HOME`, shown at the
    /// top of the sidebar. Resolved once at construction — roster's
    /// working directory does not change over a session — and `None` when
    /// it can't be read, which simply omits the sidebar's title rows.
    workspace: Option<String>,
    /// Panes the user pinned to the top of the sidebar, overriding triage
    /// (raw pane ids). Session-only, main-thread-owned — unlike
    /// `auto_approve`, no hook thread touches it, so a plain set suffices.
    /// Lights the card's pin marker and drives [`pin_to_top`]'s reorder.
    pinned: HashSet<u64>,
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
        let launchables = launch_items(&detector, &default_shell());
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
            last_shells: Vec::new(),
            last_detect: Instant::now() - DETECT_EVERY,
            last_area: Rect::new(0, 0, SPAWN_COLS, SPAWN_ROWS),
            dragging: None,
            card_drag: None,
            placeholder: None,
            next_generation: 0,
            zoomed: true,
            last_mouse: None,
            last_click: None,
            pointer: Pointer::Default,
            toasts: Vec::new(),
            sel_drag: None,
            mouse_fwd: None,
            selection: None,
            remote: None,
            quit: false,
            output_tx,
            output_rx,
            hook_sock,
            auto_approve,
            rate_limits: None,
            rate_limits_at: None,
            limit_notifier: LimitNotifier::new(),
            workspace: current_workspace(),
            pinned: HashSet::new(),
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
        let launchables = launch_items(&detector, &default_shell());
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
            last_shells: Vec::new(),
            last_detect: Instant::now() - DETECT_EVERY,
            last_area: Rect::new(0, 0, SPAWN_COLS, SPAWN_ROWS),
            dragging: None,
            card_drag: None,
            placeholder,
            next_generation: 0,
            zoomed: true,
            last_mouse: None,
            last_click: None,
            pointer: Pointer::Default,
            toasts: Vec::new(),
            sel_drag: None,
            mouse_fwd: None,
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
            rate_limits: None,
            rate_limits_at: None,
            limit_notifier: LimitNotifier::new(),
            workspace: current_workspace(),
            pinned: HashSet::new(),
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
        self.drop_selection(id);
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
                hook_activity: None,
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
        self.drop_selection(id);
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
                hook_activity: None,
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
            self.tick_drag_selection();
            self.relay_clipboard_writes();
            self.detect_if_due();
            self.toasts.retain(|toast| toast.born.elapsed() < TOAST_TTL);
            self.sync_remote_layout();

            self.last_entries = sidebar_entries(&self.session, &self.detector, Instant::now());
            // The bare-start backdrop shell is scenery, not a tenant — it
            // must never appear as a shells-section row on the welcome
            // screen (see `placeholder`'s doc).
            self.last_shells = shell_entries(&self.session, &self.detector)
                .into_iter()
                .filter(|shell| Some(shell.pane) != self.placeholder)
                .collect();
            // Light the `auto` chip from the shared set (a poisoned lock
            // just leaves chips unlit). Different fields than last_entries, so
            // the borrows are disjoint.
            if let Ok(set) = self.auto_approve.lock() {
                for entry in &mut self.last_entries {
                    entry.auto_approve = set.contains(&entry.pane.raw());
                }
            }
            // Pinned cards float above the triage order the entries arrived
            // in — the flag lights the marker, the stable reorder does the
            // override. Render and hit-testing both read this reordered
            // list, so their row plans stay in lockstep.
            for entry in &mut self.last_entries {
                entry.pinned = self.pinned.contains(&entry.pane.raw());
            }
            pin_to_top(&mut self.last_entries);
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
            // Hover follows the last known pointer position; the modal
            // overlays (launcher, confirm, context menu) own hover themselves
            // while open.
            let hover = match self.mode {
                Mode::Launch(_) | Mode::ConfirmClose(_) | Mode::ContextMenu { .. } => None,
                _ => self.last_mouse.map(|(x, y)| self.hit_at(x, y)),
            };
            let (mode_badge, mut status) = self.status_line();
            // Right-click has no drawn target, so surface it where it's
            // relevant: while the pointer rests on a card, the otherwise-
            // silent Normal footer names the gesture. Contextual, so the
            // at-rest footer stays quiet; absent on click-only terminals
            // that never report motion (hover is never set there), where
            // right-click still works — the same graceful degradation as
            // the hover-revealed `auto` chip.
            if matches!(self.mode, Mode::Normal)
                && matches!(hover, Some(Hit::SidebarEntry(_) | Hit::SidebarAuto(_)))
            {
                status.push_str(" · right-click: actions");
            }
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
            // The menu's items outlive this borrow: bound here so the View
            // can hold a slice into them for the draw below.
            let context_menu_items = match &self.mode {
                Mode::ContextMenu { pane, .. } => self.context_menu_items(*pane),
                _ => Vec::new(),
            };
            let context_menu = match &self.mode {
                Mode::ContextMenu { anchor, .. } => {
                    let hover = self.last_mouse.and_then(|(x, y)| {
                        menu_item_at(self.last_area, *anchor, &context_menu_items, x, y)
                    });
                    Some(ContextMenuView {
                        items: context_menu_items.as_slice(),
                        anchor: *anchor,
                        hover,
                    })
                }
                _ => None,
            };
            let toast_view: Vec<(&str, ToastLevel)> = self
                .toasts
                .iter()
                .map(|toast| (toast.text.as_str(), toast.level))
                .collect();
            // The selection lives in absolute rows; the renderer wants
            // viewport cells at the pane's current scroll position, with
            // off-screen spans clipped away.
            let selection = self.selection.and_then(|(id, a, b)| {
                let rt = self.runtimes.get(&id)?;
                let (cols, rows) = rt.screen.size();
                let visible = viewport_selection(
                    a,
                    b,
                    rt.screen.history_size(),
                    rt.screen.display_offset(),
                    cols,
                    rows,
                )?;
                Some((id, visible.0, visible.1))
            });
            let scrolled: HashMap<PaneId, usize> = self
                .runtimes
                .iter()
                .filter_map(|(id, rt)| {
                    let offset = rt.screen.display_offset();
                    (offset > 0).then_some((*id, offset))
                })
                .collect();
            let clock = local_clock(SystemTime::now());
            let view = View {
                session: &self.session,
                grids: &grids,
                exited: &exited,
                entries: &self.last_entries,
                shells: &self.last_shells,
                selected,
                hover,
                zoomed: self.zoomed,
                side: self.side,
                launcher,
                confirm,
                context_menu,
                toasts: &toast_view,
                rate_limits: self.rate_limits.as_ref(),
                selection,
                scrolled: &scrolled,
                welcome: self.placeholder.is_some(),
                mode_badge,
                status: &status,
                // ~8 spinner frames per second, derived from wall time so
                // the cadence is stable regardless of input polling.
                tick: started.elapsed().as_millis() as u64 / 125,
                workspace: self.workspace.as_deref(),
                clock: Some(&clock),
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
                Output::Hook(Frame::HookActivity { pane, tool, reason }) => {
                    self.apply_hook_activity(pane, tool, reason);
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

    /// A tool started (`PreToolUse`): it both answers a matching ask and
    /// announces the pane's current activity. Clears a blocked pin for the
    /// same tool (like [`apply_hook_clear`] — a mismatched parallel tool must
    /// not erase a pending ask), then records the activity as the working
    /// reason. The state is left to the scrape: the reading-apply path shows
    /// this reason only while the screen reads working, so a started tool
    /// whose ask is still genuinely pending (different tool) never masquerades
    /// as working. Same gates as a blocked pin: identified, live panes only.
    fn apply_hook_activity(&mut self, pane: u64, tool: String, reason: String) {
        let id = PaneId::from_raw(pane);
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.kind.is_none() || rt.exited.is_some() {
            return;
        }
        // Tool-matched clear of a pending ask, identical to a hook clear.
        if rt
            .hook_blocked
            .as_ref()
            .is_some_and(|pin| tool.is_empty() || pin.tool == tool)
        {
            rt.hook_blocked = None;
        }
        // A still-pending ask for another tool outranks the activity: don't
        // let a parallel auto-approved tool paint the card working while the
        // human still owes an answer.
        if rt.hook_blocked.is_some() {
            return;
        }
        let mut reason = reason;
        if reason.chars().count() > HOOK_REASON_CAP {
            reason = reason.chars().take(HOOK_REASON_CAP).collect();
        }
        rt.hook_activity = Some(ActivityPin {
            reason,
            at: Instant::now(),
        });
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
        let Some(payload) = roster_detect::statusline::parse(json) else {
            return;
        };
        // One payload, two lifetimes: the numbers ride the tracker and age
        // out; the session identity and name go to the session model, which
        // owns their stickiness (see `Session::set_session_name`). A
        // numbers-less payload leaves the tracker untouched so it cannot
        // clobber a fresh reading.
        if let Some(telemetry) = payload.telemetry {
            rt.tracker.set_telemetry(telemetry, Instant::now());
        }
        self.session.set_session_name(
            PaneId::from_raw(pane),
            payload.session_id,
            payload.session_name,
        );
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
            // End of turn (empty tool) also retires the working activity, so
            // the last tool call can't linger as a stale reason once the
            // agent stops. A tool-specific clear leaves it — the turn goes on.
            if tool.is_empty() {
                rt.hook_activity = None;
            }
        }
    }

    /// Arm a freshly spawned agent pane when the fleet `auto-yes` toggle is
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
        // The toast names the pane with the card's own fallback chain —
        // live title, then the session's statusline name, then the generic
        // agent name: cards re-sort as states change, so the feedback must
        // say which agent actually toggled, and it must say it with the
        // same name the card wears.
        let id = PaneId::from_raw(pane);
        let name = self
            .runtimes
            .get(&id)
            .and_then(|rt| rt.screen.title())
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
            .or_else(|| self.session.pane(id).and_then(|p| p.session_name.clone()))
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
                Frame::HookActivity { pane, tool, reason } => {
                    self.apply_hook_activity(pane, tool, reason)
                }
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
        let layout = match self.zoomed_pane() {
            Some(id) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
            None => self.session.layout(panes.width, panes.height),
        };
        for (id, rect) in layout {
            let content = content_rect(rect);
            if content.width == 0 || content.height == 0 {
                continue;
            }
            let Some(rt) = self.runtimes.get_mut(&id) else {
                continue;
            };
            if rt.screen.size() != (content.width, content.height) {
                rt.screen.resize(content.width, content.height);
                let _ = rt.io.resize(content.width, content.height);
                // Reflow rewrites history line boundaries, so the
                // selection's absolute rows now name different text —
                // drop it rather than highlight (or copy) the wrong lines.
                self.drop_selection(id);
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
            // The live terminal title labels the pane's card, for agents and
            // shells alike — whoever owns the PTY is the only one who can say
            // what it is doing. A reset title clears it so the card falls back
            // to the agent name (or the command's basename) rather than a
            // stale task.
            let title = rt
                .screen
                .title()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            self.session.set_title(*id, title);
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
            // A working card's reason: the hook's current tool call is richer
            // than the scraped spinner ("Bash: cargo test" over "✱
            // Deliberating…"), but only while the screen agrees the pane is
            // working — the screen owns the state. Once a settled reading
            // leaves working, the activity is stale: drop it (past the paint
            // grace, so a frame racing a just-started tool can't clear it),
            // and let the scrape's own reason stand.
            let reason = match &rt.hook_activity {
                Some(act) if reading.state == AgentState::Working => Some(act.reason.clone()),
                _ => reading.reason,
            };
            if reading.state != AgentState::Working
                && rt
                    .hook_activity
                    .as_ref()
                    .is_some_and(|act| now.duration_since(act.at) > HOOK_PIN_GRACE)
            {
                rt.hook_activity = None;
            }
            self.session.set_reading(*id, reading.state, reason, now);
        }
        // Rate limits are account-scoped, so the sidebar footer wants one
        // fleet reading, not a per-card one: merge the panes' stamped
        // telemetry, freshest window first. The gate matches the loop
        // above (`kind` cleared on exit takes a pane out), and the readings
        // come from the trackers that just purged stale payloads — the
        // footer's *live* input can never assert what the cards no longer
        // do.
        let live = fleet_rate_limit(
            self.runtimes
                .values()
                .filter(|rt| rt.kind.is_some())
                .filter_map(|rt| rt.tracker.telemetry_stamped())
                .filter_map(|(telemetry, at)| {
                    telemetry.rate_limit.as_ref().map(|limit| (limit, at))
                }),
        );
        // Claude Code re-runs the statusline only while the conversation
        // moves, so an idle fleet's feeds go quiet for far longer than the
        // trackers' 30s ageout — carry the previous fleet reading across
        // the gap instead of blanking the footer.
        let agents_present = self.runtimes.values().any(|rt| rt.kind.is_some());
        (self.rate_limits, self.rate_limits_at) = carry_tick(
            live,
            (self.rate_limits.take(), self.rate_limits_at),
            agents_present,
            now,
            SystemTime::now(),
        );
        // The notifier owns the edge state: one toast per threshold per
        // window, re-armed when usage falls back or the window resets. The
        // wording is the TUI's (`limit_notice_text`); only the loudness is
        // picked here.
        let notices = self.limit_notifier.observe(self.rate_limits.as_ref());
        for notice in notices {
            let level = match notice.alert {
                ContextAlert::Warn => ToastLevel::Warn,
                // 90% is the loud tier: the red, read-me treatment.
                ContextAlert::Critical => ToastLevel::Error,
            };
            self.toast(roster_tui::limit_notice_text(&notice), level);
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
        let content_local = content_rect(rect);
        Some(Rect::new(
            panes.x + content_local.x,
            panes.y + content_local.y,
            content_local.width,
            content_local.height,
        ))
    }

    /// The selection endpoint under an absolute pointer position: clamped
    /// into the pane's content rect (a drag past the border still selects
    /// the border row) and converted to the pane's scrollback-absolute
    /// rows at its current scroll position. `None` when the pane isn't
    /// visible or has no runtime.
    fn pointer_sel_point(&self, id: PaneId, x: u16, y: u16) -> Option<SelPoint> {
        let content = self.pane_content_rect(id)?;
        let rt = self.runtimes.get(&id)?;
        let cx = x.clamp(content.x, content.x + content.width.saturating_sub(1));
        let cy = y.clamp(content.y, content.y + content.height.saturating_sub(1));
        Some((
            cx - content.x,
            absolute_row(
                cy - content.y,
                rt.screen.history_size(),
                rt.screen.display_offset(),
            ),
        ))
    }

    /// Drop the selection and any in-progress drag that belong to `id` —
    /// the shared invalidation for every event that re-keys or replaces
    /// the pane's buffer (close, reattach, resize reflow). A held mouse
    /// grab forwarding to the pane's guest dies with it.
    fn drop_selection(&mut self, id: PaneId) {
        if self.selection.map(|s| s.0) == Some(id) {
            self.selection = None;
        }
        if self.sel_drag.map(|d| d.pane) == Some(id) {
            self.sel_drag = None;
        }
        if self.mouse_fwd == Some(id) {
            self.mouse_fwd = None;
        }
    }

    /// Whether the pane's guest gets the real mouse: alive, and it
    /// negotiated mouse tracking in SGR encoding (Claude Code turns on
    /// DECSET 1000/1002/1003 + 1006 and runs its own drag-selection; a
    /// legacy-encoding guest would misread the reports).
    fn guest_takes_mouse(&self, id: PaneId) -> bool {
        self.runtimes.get(&id).is_some_and(|rt| {
            rt.exited.is_none() && rt.screen.mouse_reporting() && rt.screen.sgr_mouse()
        })
    }

    /// Pane-local, 1-based cell coordinates for a forwarded mouse report,
    /// clamped into the pane's content rect. `None` when the pane isn't
    /// visible in the active window.
    fn pane_local_cell(&self, id: PaneId, x: u16, y: u16) -> Option<(u16, u16)> {
        let c = self.pane_content_rect(id)?;
        let col = x.saturating_sub(c.x).min(c.width.saturating_sub(1)) + 1;
        let row = y.saturating_sub(c.y).min(c.height.saturating_sub(1)) + 1;
        Some((col, row))
    }

    /// Forward a left-button event to `id`'s guest as an SGR report at the
    /// pointer's pane-local cell. Best-effort: a dead pipe just drops it.
    fn forward_mouse(&mut self, id: PaneId, x: u16, y: u16, kind: MouseEventKind) {
        let Some((col, row)) = self.pane_local_cell(id, x, y) else {
            return;
        };
        let Some(report) = sgr_left(col, row, kind) else {
            return;
        };
        if let Some(rt) = self.runtimes.get_mut(&id) {
            let _ = rt.io.write(&report);
        }
    }

    /// Relay the focused pane's OSC 52 clipboard writes out to the hosting
    /// terminal — a mouse-native guest (Claude Code) copies its own
    /// drag-selection this way, and without the relay the copy would die
    /// inside the emulator. Every pane's queue drains each frame so a
    /// looping guest can't pile up payloads, but only the focused pane
    /// reaches the real clipboard: a background pane silently replacing
    /// what the user copied is a hijack, not a copy — and the gesture that
    /// matters (a drag in the pane) focuses it first. One sweep covers
    /// local and remote panes alike; the queues are empty in the common
    /// frame.
    fn relay_clipboard_writes(&mut self) {
        let focused = self.session.focused();
        for (id, rt) in self.runtimes.iter_mut() {
            let writes = rt.screen.take_clipboard_writes();
            if Some(*id) == focused {
                for text in writes {
                    copy_to_clipboard(&text);
                }
            }
        }
    }

    /// Per-frame upkeep of an in-progress drag-selection. Drops the drag
    /// (and its selection) when the pane is gone or flipped between the
    /// primary and alternate screens — either re-keys the absolute rows,
    /// and copying remapped text would be worse than losing the drag.
    /// Otherwise, while the drag is held past the pane's top or bottom
    /// content edge, scrolls that pane's history one step toward the
    /// pointer and re-extends the selection — drag events only arrive
    /// while the mouse moves, so the render loop drives this for a
    /// held-still pointer. Rate-limited by [`DRAG_SCROLL_EVERY`]; inert
    /// while a modal owns the mouse and before the drag has produced a
    /// selection, so a held click on an edge row never scrolls.
    fn tick_drag_selection(&mut self) {
        let Some(drag) = self.sel_drag else {
            return;
        };
        let id = drag.pane;
        let alive = self
            .runtimes
            .get(&id)
            .is_some_and(|rt| rt.screen.alternate_screen() == drag.alt);
        if !alive {
            self.sel_drag = None;
            self.selection = None;
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        // The alternate screen has no history to scroll — the guest owns
        // its scrollback (Claude Code virtualizes its transcript). The drag
        // itself stays exactly as before, but a hold past the edge says so
        // once instead of failing silently.
        if drag.alt {
            if !drag.hinted
                && self.last_mouse.zip(self.pane_content_rect(id)).is_some_and(
                    |((_, y), content)| edge_scroll_delta(y, content.y, content.height) != 0,
                )
            {
                self.toast(
                    "this app scrolls itself — selection covers the screen".to_string(),
                    ToastLevel::Info,
                );
                if let Some(drag) = &mut self.sel_drag {
                    drag.hinted = true;
                }
            }
            return;
        }
        if self.selection.map(|s| s.0) != Some(id) {
            return;
        }
        let Some((x, y)) = self.last_mouse else {
            return;
        };
        if drag
            .scrolled
            .is_some_and(|at| at.elapsed() < DRAG_SCROLL_EVERY)
        {
            return;
        }
        let Some(content) = self.pane_content_rect(id) else {
            return;
        };
        let delta = edge_scroll_delta(y, content.y, content.height);
        if delta == 0 {
            return;
        }
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        rt.screen.scroll_display(delta);
        if let Some(drag) = &mut self.sel_drag {
            drag.scrolled = Some(Instant::now());
        }
        // The pointer sits still while the text moves under it: re-read the
        // endpoint so the selection keeps extending.
        if let Some(end) = self.pointer_sel_point(id, x, y) {
            self.selection = Some((id, drag.anchor, end));
        }
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
            &HitContext {
                limits: self.rate_limits.as_ref(),
                zoomed: self.zoomed_pane(),
                workspace_header: self.workspace.is_some(),
                shells: &self.last_shells,
            },
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
                    // Unshifted split keys named for the outcome, not the axis:
                    // r puts the new pane to the right, b puts it below. The b
                    // arm excludes ctrl so ctrl-b still passes the prefix
                    // through (the arm below), independent of match order.
                    KeyCode::Char('r') => self.split(SplitDirection::Horizontal),
                    KeyCode::Char('b') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.split(SplitDirection::Vertical)
                    }
                    KeyCode::Char('o') => self.session.focus_next(),
                    KeyCode::Char('n') => self.session.next_window(),
                    KeyCode::Char('p') => self.session.prev_window(),
                    KeyCode::Char('z') => self.zoomed = !self.zoomed,
                    KeyCode::Char('s') => self.side = self.side.toggled(),
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
            Mode::ContextMenu { .. } => {
                // A mouse-first popup: any keystroke dismisses it rather
                // than being swallowed into a menu with no keyboard nav.
                self.mode = Mode::Normal;
            }
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
        // The position the last drawn frame's hover affordances used. A
        // click must be judged against what that frame showed — the click
        // itself must not count as the hover that revealed a control.
        let previous_mouse = self.last_mouse;
        self.last_mouse = Some((x, y));

        // A press while a guest grab is still held means its release was
        // swallowed where this handler couldn't see it (a modal owned the
        // mouse): finish the old grab with a synthetic release before
        // anything routes the new press, or the guest keeps drag-selecting
        // a button that is no longer down.
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            if let Some(id) = self.mouse_fwd.take() {
                if self.guest_takes_mouse(id) {
                    self.forward_mouse(id, x, y, MouseEventKind::Up(MouseButton::Left));
                }
            }
        }

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

        // The context menu owns the mouse while open: an item acts, a click
        // anywhere else dismisses it, and the pointer reads as a hand over
        // its rows. Rebuilt from live pinned state each event so pin/unpin
        // can't go stale between the open and the click.
        if let Mode::ContextMenu { pane, anchor } = self.mode {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let items = self.context_menu_items(pane);
                    match menu_item_at(self.last_area, anchor, &items, x, y) {
                        Some(index) => {
                            self.mode = Mode::Normal;
                            match items[index] {
                                ContextMenuItem::Pin | ContextMenuItem::Unpin => {
                                    self.toggle_pin(pane);
                                }
                                // Same guarded path as the title's ✕: a live
                                // agent raises the confirm dialog first.
                                ContextMenuItem::Close => self.request_close(pane),
                            }
                        }
                        None => {
                            if !menu_contains(self.last_area, anchor, &items, x, y) {
                                self.mode = Mode::Normal;
                            }
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    // Right-clicking another card moves the menu to it; a
                    // right-click off any card dismisses — so the button
                    // that opens the menu also re-targets and closes it,
                    // rather than being a dead no-op while it is up.
                    self.open_context_menu(x, y);
                }
                MouseEventKind::Moved => {
                    let items = self.context_menu_items(pane);
                    set_pointer(
                        &mut self.pointer,
                        if menu_item_at(self.last_area, anchor, &items, x, y).is_some() {
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
                    // Arm a card drag; the release decides between a plain
                    // focus (dropped on the sidebar) and a move-beside
                    // (dropped on a pane). Focus is deferred so the drag can
                    // read the pre-drag layout under the drop point.
                    Hit::SidebarEntry(index) => {
                        if let Some(entry) = self.last_entries.get(index) {
                            self.card_drag = Some((entry.pane, (x, y)));
                        }
                    }
                    Hit::SidebarShell(index) => {
                        if let Some(shell) = self.last_shells.get(index) {
                            self.card_drag = Some((shell.pane, (x, y)));
                        }
                    }
                    Hit::SidebarAuto(index) => {
                        // The chip is a button on the card, not the card:
                        // toggle without stealing focus from the pane the
                        // user is watching. But only when the chip was
                        // actually drawn — armed, or its card hovered,
                        // selected, or focused (mirroring the sidebar's
                        // reveal). In a terminal that reports clicks but
                        // never motion, hover is never set and the chip
                        // stays hidden: a click there is a click on blank
                        // card space, and an invisible control must never
                        // arm auto-approve — it falls through to the
                        // card's own action, the jump.
                        if let Some(entry) = self.last_entries.get(index) {
                            let hovered = previous_mouse
                                .map(|(px, py)| self.hit_at(px, py))
                                .is_some_and(|hit| {
                                    matches!(
                                        hit,
                                        Hit::SidebarAuto(i) | Hit::SidebarEntry(i) if i == index
                                    )
                                });
                            let selected = matches!(self.mode, Mode::Jump)
                                && self.sidebar.selected(&self.last_entries) == Some(index);
                            let revealed = entry.auto_approve
                                || hovered
                                || selected
                                || self.session.focused() == Some(entry.pane);
                            let pane = entry.pane;
                            if revealed {
                                self.toggle_auto_approve(pane.raw());
                            } else {
                                self.session.focus(pane);
                            }
                        }
                    }
                    Hit::SidebarAutoAll => self.toggle_auto_all(),
                    Hit::SidebarNewAgent => {
                        self.mode = Mode::Launch(LauncherState::new());
                    }
                    Hit::SidebarToggle => self.side = self.side.toggled(),
                    Hit::StatusViewGrid => self.zoomed = false,
                    Hit::StatusViewSolo => self.zoomed = true,
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
                        // The seam between panels doubles as the split
                        // divider; grab it if it's there. Solo view has
                        // no dividers.
                        let panes = panes_area(self.last_area, self.side);
                        let mut grabbed = false;
                        if !self.zoomed && x >= panes.x && y >= panes.y {
                            let local = (x - panes.x, y - panes.y);
                            if let Some((_, at)) =
                                self.divider_under(panes.width, panes.height, local.0, local.1)
                            {
                                self.dragging = Some(at);
                                grabbed = true;
                            }
                        }
                        // Pressing on live content anchors a text
                        // selection; it only becomes one if the mouse
                        // moves before release. The anchor is taken in
                        // absolute rows so it stays glued to its text
                        // while the view scrolls.
                        if !grabbed && matches!(hit, Hit::Pane(_)) {
                            let inside = self.pane_content_rect(id).is_some_and(|content| {
                                x >= content.x
                                    && x < content.x + content.width
                                    && y >= content.y
                                    && y < content.y + content.height
                            });
                            if inside {
                                if self.guest_takes_mouse(id) {
                                    // A guest that asked for the mouse gets
                                    // the real thing: Claude Code runs its
                                    // own drag-selection over its own
                                    // scrollback and copies via OSC 52
                                    // (relayed each frame). The grab keeps
                                    // the rest of the drag on this pane.
                                    self.forward_mouse(id, x, y, mouse.kind);
                                    self.mouse_fwd = Some(id);
                                } else if let Some(anchor) = self.pointer_sel_point(id, x, y) {
                                    // Inside the rect the clamp is the
                                    // identity, so this is the press cell
                                    // itself.
                                    let alt = self
                                        .runtimes
                                        .get(&id)
                                        .is_some_and(|rt| rt.screen.alternate_screen());
                                    self.sel_drag = Some(SelectionDrag {
                                        pane: id,
                                        anchor,
                                        alt,
                                        scrolled: None,
                                        hinted: false,
                                    });
                                }
                            }
                        }
                    }
                    // Hover-only — the click has nothing to do.
                    Hit::SidebarWorkspace | Hit::Sidebar | Hit::Status | Hit::Outside => {}
                }
                self.last_click = Some((Instant::now(), (x, y)));
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click a sidebar card to open its action menu,
                // anchored at the click. The auto chip's columns are still
                // the card here: the menu — not the chip — is what the
                // right button does. Any other target opens nothing. Drop any
                // selection first, like the left-button press does.
                self.selection = None;
                self.open_context_menu(x, y);
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
                } else if let Some(id) = self.mouse_fwd {
                    // The grab routes the whole drag to the pressed pane,
                    // clamped into its content — matching what a terminal
                    // does when a drag leaves the window. Click-only
                    // tracking (DECSET 1000) never subscribed to motion,
                    // so only drag/motion-tracking guests get the moves;
                    // the release still arrives either way.
                    if self.guest_takes_mouse(id) {
                        let wants_motion = self
                            .runtimes
                            .get(&id)
                            .is_some_and(|rt| rt.screen.mouse_drag_reporting());
                        if wants_motion {
                            self.forward_mouse(id, x, y, mouse.kind);
                        }
                    } else {
                        // The guest died or turned tracking off mid-drag.
                        self.mouse_fwd = None;
                    }
                } else if let Some(drag) = self.sel_drag {
                    // Extend the selection to the cell under the pointer,
                    // clamped into the pane's content and converted to the
                    // pane's absolute rows at the current scroll position.
                    // (A pane that flipped screens since the press is
                    // dropped by the next tick; don't extend into it.)
                    let same_screen = self
                        .runtimes
                        .get(&drag.pane)
                        .is_some_and(|rt| rt.screen.alternate_screen() == drag.alt);
                    if same_screen {
                        if let Some(end) = self.pointer_sel_point(drag.pane, x, y) {
                            self.selection = Some((drag.pane, drag.anchor, end));
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = None;
                self.sel_drag = None;
                // A pressed card released: over a pane, move it in beside that
                // pane for a side-by-side view; anywhere else, it was a plain
                // click that focuses the card's pane.
                if let Some((pane, _start)) = self.card_drag.take() {
                    let onto = match self.hit_at(x, y) {
                        Hit::Pane(target)
                        | Hit::PaneTitle(target)
                        | Hit::PaneClose(target)
                        | Hit::PaneRestart(target) => Some(target),
                        _ => None,
                    };
                    let moved = onto.is_some_and(|target| {
                        self.session
                            .move_pane_beside(target, pane, SplitDirection::Horizontal)
                    });
                    if moved {
                        // Two agents share the screen now: leave solo so both
                        // show, and tuck the sidebar away for room.
                        self.zoomed = false;
                        if !self.side.is_collapsed() {
                            self.side = self.side.toggled();
                        }
                    } else {
                        self.session.focus(pane);
                    }
                    return;
                }
                // A release that ends a guest grab belongs to the guest —
                // it finishes the guest's own selection (and its own copy);
                // roster must not also re-copy a highlight elsewhere.
                if let Some(id) = self.mouse_fwd.take() {
                    if self.guest_takes_mouse(id) {
                        self.forward_mouse(id, x, y, mouse.kind);
                    }
                    return;
                }
                // A completed drag-selection copies itself — click-drag,
                // release, pasted anywhere. Extraction reads the emulator's
                // full buffer, so a selection that auto-scrolled through
                // history copies the scrolled-past lines too.
                if let Some((id, a, b)) = self.selection {
                    let text = self
                        .runtimes
                        .get(&id)
                        .map(|rt| {
                            rt.screen
                                .linear_text((usize::from(a.0), a.1), (usize::from(b.0), b.1))
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
                // Motion with no button held means the release happened
                // where we couldn't see it (outside the window, over a
                // modal): end the drag, or the edge auto-scroll would keep
                // running off a phantom button. The highlight stays; the
                // copy is lost with the release. A guest grab gets told —
                // a synthetic release — or it keeps drag-selecting a
                // button that is no longer down.
                self.sel_drag = None;
                if let Some(id) = self.mouse_fwd.take() {
                    if self.guest_takes_mouse(id) {
                        self.forward_mouse(id, x, y, MouseEventKind::Up(MouseButton::Left));
                    }
                }
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
                    let local = self.pane_local_cell(id, x, y);
                    // A held drag would be re-keyed by whatever a forwarded
                    // notch makes the guest repaint; swallowing it instead
                    // keeps the drag alive (trackpad inertia routinely lands
                    // mid-drag). Roster-side scrolls still apply — absolute
                    // rows keep the drag glued to its text through those.
                    let dragging_here = self.sel_drag.map(|d| d.pane) == Some(id);
                    let forwarded = match self.runtimes.get_mut(&id) {
                        // A dead child can't receive input, so wheel_action
                        // routes exited panes to a history scroll instead.
                        Some(rt) => match wheel_action(
                            up,
                            rt.screen.mouse_reporting(),
                            rt.screen.sgr_mouse(),
                            rt.screen.alternate_screen(),
                            rt.exited.is_none(),
                            local,
                        ) {
                            Some(WheelAction::Forward(bytes)) => {
                                !dragging_here && rt.io.write(&bytes).is_ok()
                            }
                            Some(WheelAction::Scroll(delta)) => {
                                rt.screen.scroll_display(delta);
                                false
                            }
                            None => false,
                        },
                        None => false,
                    };
                    // A wheel that reached the guest hands scrolling to it
                    // (Claude Code scrolls its own virtualized transcript):
                    // its repaint re-keys every row under a highlight, so a
                    // kept selection would end up covering different text.
                    if forwarded {
                        self.drop_selection(id);
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
                | Hit::StatusViewGrid
                | Hit::StatusViewSolo
                | Hit::SidebarEntry(_)
                | Hit::SidebarShell(_)
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
            if let Some((direction, _)) =
                self.divider_under(panes.width, panes.height, x - panes.x, y - panes.y)
            {
                return match direction {
                    SplitDirection::Horizontal => Pointer::ResizeEw,
                    SplitDirection::Vertical => Pointer::ResizeNs,
                };
            }
        }
        pointer_for(hit)
    }

    /// The divider under a pane-local position, with the cell the layout
    /// model knows it by. Under the panel chrome a split's seam is two
    /// border cells wide — the left/upper panel's border is the column the
    /// layout models as the divider, the right/lower panel's border is its
    /// twin one cell over — and both halves must grab and show the resize
    /// pointer.
    fn divider_under(
        &self,
        cols: u16,
        rows: u16,
        x: u16,
        y: u16,
    ) -> Option<(SplitDirection, (u16, u16))> {
        if let Some(direction) = self.session.divider_at(cols, rows, x, y) {
            return Some((direction, (x, y)));
        }
        if x > 0 {
            if let Some(direction @ SplitDirection::Horizontal) =
                self.session.divider_at(cols, rows, x - 1, y)
            {
                return Some((direction, (x - 1, y)));
            }
        }
        if y > 0 {
            if let Some(direction @ SplitDirection::Vertical) =
                self.session.divider_at(cols, rows, x, y - 1)
            {
                return Some((direction, (x, y - 1)));
            }
        }
        None
    }

    /// Start `command` in its own fresh window. The bare-start backdrop
    /// shell, if one exists, is replaced by this first launch regardless of
    /// focus or whether the user typed into it — the placeholder never
    /// survives past it, unlike an ordinary shell pane (the launcher's
    /// `shell` row, a split), which is a supported tenant and keeps
    /// running.
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
        // Solo shows one pane; a split you can't see is useless, so making a
        // second pane drops to the grid to reveal both halves.
        self.zoomed = false;
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

    /// Open (or move) the card context menu for whatever sidebar card sits
    /// at (`x`, `y`); a non-card target, or a frame too small to draw the
    /// menu, closes any open menu instead of entering a mode with nothing on
    /// screen. Shared by the right-click open and the re-anchor while open,
    /// so both resolve the target the same way.
    fn open_context_menu(&mut self, x: u16, y: u16) {
        self.mode = Mode::Normal;
        let (Hit::SidebarEntry(index) | Hit::SidebarAuto(index)) = self.hit_at(x, y) else {
            return;
        };
        let Some(entry) = self.last_entries.get(index) else {
            return;
        };
        let pane = entry.pane;
        let anchor = (x, y);
        if menu_fits(self.last_area, anchor, &self.context_menu_items(pane)) {
            self.mode = Mode::ContextMenu { pane, anchor };
        }
    }

    /// The context-menu actions for `pane`: pin or unpin depending on its
    /// current state, then the close action.
    fn context_menu_items(&self, pane: PaneId) -> Vec<ContextMenuItem> {
        let pin = if self.pinned.contains(&pane.raw()) {
            ContextMenuItem::Unpin
        } else {
            ContextMenuItem::Pin
        };
        vec![pin, ContextMenuItem::Close]
    }

    /// Toggle whether `pane` is pinned to the top of the sidebar. Local and
    /// session-only; the next frame's [`pin_to_top`] reorder picks it up.
    fn toggle_pin(&mut self, pane: PaneId) {
        if !self.pinned.remove(&pane.raw()) {
            self.pinned.insert(pane.raw());
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        if self.placeholder == Some(id) {
            self.placeholder = None;
        }
        // Drop any pin so a later pane reusing this id can't inherit it.
        self.pinned.remove(&id.raw());
        self.drop_selection(id);
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
        // A paste is guest input like typing: the echo repaints the pane, so
        // a selection (or held drag) there would end up over different text.
        self.selection = None;
        self.sel_drag = None;
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
        // Typing drops the selection — and any drag feeding it, or the next
        // pointer move would resurrect the highlight from the old anchor —
        // and snaps back to live output.
        self.selection = None;
        self.sel_drag = None;
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

    /// The status line: a badge while a mode is armed (the modal modes
    /// skip it — the modal announces itself), plus contextual key hints.
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
                // At rest the footer stays nearly silent: where keystrokes
                // go, plus the one key that opens everything else. The
                // full palette lives on the PREFIX row — hold ctrl-b and
                // it appears.
                if self.zoomed {
                    (
                        Some("SOLO"),
                        format!("{focused}click a card to switch · ctrl-b: keys · then z: grid"),
                    )
                } else {
                    (None, format!("{focused}ctrl-b: keys · then j: jump"))
                }
            }
            Mode::Prefix => (
                Some("PREFIX"),
                "c: new agent · n/p: windows · z: solo · s: sidebar · r: split right · b: split below · o: next pane · j: jump · x: close agent · d: detach · q: quit roster"
                    .to_string(),
            ),
            Mode::Jump => (
                Some("JUMP"),
                "j/k: move · enter: jump to pane · a: auto-approve · esc: cancel".to_string(),
            ),
            // The modal modes carry no badge: the modal itself announces
            // the mode, and a corner pill reads as a second (dead) button.
            Mode::Launch(_) => (
                None,
                "type to filter or run a command · enter: launch · tab: edit flags · esc: cancel"
                    .to_string(),
            ),
            Mode::ConfirmClose(_) => (None, "y/enter: close · esc: cancel".to_string()),
            Mode::ContextMenu { .. } => {
                (None, "click an action · esc: cancel".to_string())
            }
        }
    }
}

fn is_prefix(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// A selection endpoint: content-local column and scrollback-absolute row
/// (see [`absolute_row`]).
type SelPoint = (u16, usize);

/// An in-progress mouse drag-selection: where it anchored, plus the upkeep
/// state that must end with the drag.
#[derive(Clone, Copy)]
struct SelectionDrag {
    /// The pane being selected in.
    pane: PaneId,
    /// The anchor endpoint, fixed at press time.
    anchor: SelPoint,
    /// Whether the pane was on the alternate screen at press time. A flip
    /// mid-drag re-keys every absolute row (the alternate screen has no
    /// history), so the drag is dropped rather than copy remapped text.
    alt: bool,
    /// When the edge auto-scroll last stepped — the rate limit that keeps
    /// an event-burst frame rate from becoming a scroll burst.
    scrolled: Option<Instant>,
    /// Whether this drag already explained that a self-scrolling pane
    /// can't edge-scroll — the toast fires once per drag, not per tick.
    hinted: bool,
}

/// Rows a drag auto-scroll steps per tick — modest, so the selection stays
/// readable while it grows.
const DRAG_SCROLL_ROWS: i32 = 2;

/// Minimum time between drag auto-scroll steps. The render loop ticks
/// faster under an event burst; this keeps the scroll rate steady.
const DRAG_SCROLL_EVERY: Duration = Duration::from_millis(50);

/// The buffer row under a viewport row, counted from the top of scrollback
/// history: row 0 is the oldest kept line, row `history_size` the top of
/// the live screen. `display_offset` counts up from the bottom, so the
/// viewport's top row sits `history_size - display_offset` rows into the
/// buffer. Absolute rows stay pinned to their text as output grows —
/// history grows by exactly what the screen sheds — and drift only when
/// the scrollback cap trims the oldest lines.
fn absolute_row(viewport_row: u16, history_size: usize, display_offset: usize) -> usize {
    history_size.saturating_sub(display_offset) + usize::from(viewport_row)
}

/// The visible part of an absolute selection at the current scroll
/// position, as viewport cells for the renderer. The span is linear
/// (reading order), so an endpoint scrolled off the top clips to the
/// viewport's first cell and one scrolled off the bottom to its last —
/// exactly the cells of the span still on screen. `None` when the whole
/// selection is out of view or the viewport is degenerate.
fn viewport_selection(
    a: SelPoint,
    b: SelPoint,
    history_size: usize,
    display_offset: usize,
    cols: u16,
    rows: u16,
) -> Option<((u16, u16), (u16, u16))> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let (mut a, mut b) = (a, b);
    // Normalize to reading order: (col, row) sorts by row, then col.
    if (a.1, a.0) > (b.1, b.0) {
        std::mem::swap(&mut a, &mut b);
    }
    let top = absolute_row(0, history_size, display_offset);
    let bottom = top + usize::from(rows);
    if b.1 < top || a.1 >= bottom {
        return None;
    }
    let start = if a.1 < top {
        (0, 0)
    } else {
        (a.0, (a.1 - top) as u16)
    };
    let end = if b.1 >= bottom {
        (cols - 1, rows - 1)
    } else {
        (b.0, (b.1 - top) as u16)
    };
    Some((start, end))
}

/// The auto-scroll step for a drag whose pointer sits at `pointer_y`
/// against a content rect spanning `top..top + height`: positive (into
/// history) above the content, negative below it, zero anywhere inside.
/// Content rows themselves — including the first and last — never scroll,
/// so text on them stays precisely selectable; the pane's border row is
/// always reachable as the trigger because content is inset from the
/// panel frame.
fn edge_scroll_delta(pointer_y: u16, top: u16, height: u16) -> i32 {
    if height == 0 {
        0
    } else if pointer_y < top {
        DRAG_SCROLL_ROWS
    } else if pointer_y >= top + height {
        -DRAG_SCROLL_ROWS
    } else {
        0
    }
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

/// Encode a left-button event as an SGR mouse report (DECSET 1006) at
/// 1-based `(col, row)`: button 0 on press (`M`) and release (`m`), +32
/// while moving with the button held. `None` for events that aren't the
/// left button — only the primary button forwards. Callers must confirm
/// the guest negotiated SGR first.
fn sgr_left(col: u16, row: u16, kind: MouseEventKind) -> Option<Vec<u8>> {
    let (button, suffix) = match kind {
        MouseEventKind::Down(MouseButton::Left) => (0, 'M'),
        MouseEventKind::Drag(MouseButton::Left) => (32, 'M'),
        MouseEventKind::Up(MouseButton::Left) => (0, 'm'),
        _ => return None,
    };
    Some(format!("\x1b[<{button};{col};{row}{suffix}").into_bytes())
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
                        Ok(Some(
                            frame @ (Frame::HookClear { .. }
                            | Frame::HookActivity { .. }
                            | Frame::Statusline { .. }),
                        )) => {
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

    /// A one-window rate limit for driving the carry.
    fn five_limit(used_pct: f32, resets_in: Duration) -> RateLimit {
        RateLimit {
            five_hour: Some(roster_core::RateLimitWindow {
                used_pct,
                resets_in: Some(resets_in),
            }),
            seven_day: None,
        }
    }

    /// The five-hour countdown inside a carry result.
    fn resets(limits: &Option<RateLimit>) -> Duration {
        limits
            .as_ref()
            .and_then(|l| l.five_hour.as_ref())
            .and_then(|w| w.resets_in)
            .expect("five-hour countdown present")
    }

    #[test]
    fn the_carry_accumulates_aging_across_quiet_ticks() {
        let t0 = Instant::now();
        let w0 = SystemTime::now();
        let live = five_limit(75.0, Duration::from_secs(7200));

        // A live tick seeds the carry; the reading displays as it arrived.
        let held = carry_tick(Some(live), (None, None), true, t0, w0);
        assert_eq!(resets(&held.0), Duration::from_secs(7200));
        assert_eq!(held.1, Some((t0, w0)));

        // Two quiet ticks ten minutes apart: aging sums across the
        // re-stamps, it does not restart from each tick's value.
        let (t1, w1) = (t0 + Duration::from_secs(600), w0 + Duration::from_secs(600));
        let held = carry_tick(None, held, true, t1, w1);
        assert_eq!(resets(&held.0), Duration::from_secs(6600));
        let (t2, w2) = (t1 + Duration::from_secs(600), w1 + Duration::from_secs(600));
        let held = carry_tick(None, held, true, t2, w2);
        assert_eq!(resets(&held.0), Duration::from_secs(6000));

        // Quiet past the reset horizon: the window dies and the stamps
        // clear with it — no corpse for a later tick to age.
        let (t3, w3) = (
            t2 + Duration::from_secs(6001),
            w2 + Duration::from_secs(6001),
        );
        assert_eq!(carry_tick(None, held, true, t3, w3), (None, None));
    }

    #[test]
    fn the_carry_clears_when_no_agent_pane_remains() {
        let t0 = Instant::now();
        let w0 = SystemTime::now();
        let held = (
            Some(five_limit(91.0, Duration::from_secs(7200))),
            Some((t0, w0)),
        );
        // Every agent pane exited: a footer asserting account limits over
        // an agentless session would outlive its own subjects.
        assert_eq!(
            carry_tick(None, held, false, t0 + Duration::from_secs(1), w0),
            (None, None)
        );
    }

    #[test]
    fn a_sleep_gap_ages_the_carry_by_the_wall_clock() {
        let t0 = Instant::now();
        let w0 = SystemTime::now();
        let held = (
            Some(five_limit(92.0, Duration::from_secs(1800))),
            Some((t0, w0)),
        );
        // The machine slept two hours: the monotonic clock barely moved,
        // the wall clock did — the window's reset passed mid-sleep, so it
        // must retire, not resume its countdown where the sleep paused it.
        let (limits, stamps) = carry_tick(
            None,
            held,
            true,
            t0 + Duration::from_millis(400),
            w0 + Duration::from_secs(7200),
        );
        assert_eq!(limits, None);
        assert_eq!(stamps, None);
    }

    #[test]
    fn a_backward_wall_step_falls_back_to_the_monotonic_clock() {
        let t0 = Instant::now();
        let w0 = SystemTime::now();
        let held = (
            Some(five_limit(75.0, Duration::from_secs(7200))),
            Some((t0, w0)),
        );
        // The wall clock stepped backwards (NTP): elapsed comes from the
        // monotonic side, and the countdown neither freezes nor rewinds.
        let (limits, _) = carry_tick(
            None,
            held,
            true,
            t0 + Duration::from_secs(60),
            w0 - Duration::from_secs(3600),
        );
        assert_eq!(resets(&limits), Duration::from_secs(7140));
    }

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
    fn absolute_rows_pin_viewport_cells_to_history() {
        // Live at the bottom: the viewport sits directly under history.
        assert_eq!(absolute_row(0, 100, 0), 100);
        assert_eq!(absolute_row(5, 100, 0), 105);
        // Scrolled up: the same viewport row reads earlier buffer lines.
        assert_eq!(absolute_row(0, 100, 30), 70);
        // Scrolled to history's very top, the first kept line is row 0.
        assert_eq!(absolute_row(0, 100, 100), 0);
        // No history (a fresh pane, or the alternate screen): absolute rows
        // are plain viewport rows, so behavior degrades to exactly the old
        // visible-grid coordinates.
        assert_eq!(absolute_row(2, 0, 0), 2);
        // An offset beyond history can't underflow (defensive; alacritty
        // clamps offsets to the history extent).
        assert_eq!(absolute_row(1, 3, 9), 1);
    }

    #[test]
    fn viewport_selection_clips_offscreen_endpoints() {
        // A 10x4 viewport over 100 history lines, scrolled up 20: showing
        // absolute rows 80..84.
        let show = |a, b| viewport_selection(a, b, 100, 20, 10, 4);
        // Fully visible spans convert row-by-row, either endpoint order.
        assert_eq!(show((2, 81), (7, 83)), Some(((2, 1), (7, 3))));
        assert_eq!(show((7, 83), (2, 81)), Some(((2, 1), (7, 3))));
        // A start scrolled off the top clips to the viewport's first cell —
        // the linear span covers everything from the top-left on.
        assert_eq!(show((4, 10), (7, 82)), Some(((0, 0), (7, 2))));
        // An end scrolled off the bottom clips to the last cell.
        assert_eq!(show((4, 82), (3, 99)), Some(((4, 2), (9, 3))));
        // Both off: the whole viewport is inside the span.
        assert_eq!(show((4, 10), (3, 99)), Some(((0, 0), (9, 3))));
        // Boundary rows: the viewport's own first and last rows convert.
        assert_eq!(show((0, 80), (9, 83)), Some(((0, 0), (9, 3))));
    }

    #[test]
    fn viewport_selection_hides_a_fully_offscreen_span() {
        let show = |a, b| viewport_selection(a, b, 100, 20, 10, 4);
        // Entirely above the view, entirely below, and the row just past
        // each boundary.
        assert_eq!(show((0, 10), (9, 79)), None);
        assert_eq!(show((0, 84), (9, 99)), None);
        // Degenerate viewports never render a selection.
        assert_eq!(viewport_selection((0, 0), (9, 9), 10, 0, 0, 4), None);
        assert_eq!(viewport_selection((0, 0), (9, 9), 10, 0, 10, 0), None);
    }

    #[test]
    fn drags_scroll_only_past_the_content_edges() {
        // A content rect spanning rows 5..15 (top 5, height 10).
        // Above the content (the pane border row and beyond): scroll up.
        assert!(edge_scroll_delta(4, 5, 10) > 0);
        assert!(edge_scroll_delta(0, 5, 10) > 0);
        // Below the content: scroll back down.
        assert!(edge_scroll_delta(15, 5, 10) < 0);
        assert!(edge_scroll_delta(30, 5, 10) < 0);
        // Content rows never scroll — the first and last visible rows
        // must stay precisely selectable, not run away under the drag.
        assert_eq!(edge_scroll_delta(5, 5, 10), 0);
        assert_eq!(edge_scroll_delta(14, 5, 10), 0);
        assert_eq!(edge_scroll_delta(9, 5, 10), 0);
        // Degenerate rects never scroll.
        assert_eq!(edge_scroll_delta(5, 5, 0), 0);
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
