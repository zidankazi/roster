//! The event loop: PTY output → emulator → detection → model → repaint,
//! plus key handling and the pane-switch side effects.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

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
    content_rect, hit_test, launch_items, local_panes, panes_area, pointer_for, render,
    sidebar_entries, Hit, LaunchItem, Launcher, LauncherState, Message, Pointer, SidebarEntry,
    SidebarSide, SidebarState, View,
};

use crate::keys::encode_key;

/// How long to wait for input before running another frame.
const INPUT_POLL: Duration = Duration::from_millis(50);
/// How often detection re-reads each pane's screen.
const DETECT_EVERY: Duration = Duration::from_millis(400);
/// Size panes are spawned at before the first layout pass corrects them.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

/// Bytes or end-of-stream from a pane's reader thread. The generation says
/// which attachment produced it: replacing a pane's command reuses the pane
/// id, and the dying process's tail (and EOF) must not touch its successor.
enum Output {
    Bytes(PaneId, u64, Vec<u8>),
    Eof(PaneId, u64),
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
}

/// Everything one live pane owns besides its model entry.
struct PaneRuntime {
    pty: Pty,
    screen: Screen,
    tracker: PaneTracker,
    kind: Option<AgentKind>,
    /// Which attachment this is; output from older generations is stale.
    generation: u64,
    /// Exit code, once the child has ended; the pane stays visible.
    exited: Option<u32>,
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
    /// The bare-start shell pane, until the user actually uses it. It only
    /// exists as a backdrop for the launcher, so the first launch replaces
    /// it instead of splitting it.
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
    notice: Option<String>,
    quit: bool,
    output_tx: Sender<Output>,
    output_rx: Receiver<Output>,
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
            last_detect: Instant::now() - DETECT_EVERY,
            last_area: Rect::new(0, 0, SPAWN_COLS, SPAWN_ROWS),
            dragging: None,
            placeholder: None,
            next_generation: 0,
            zoomed: false,
            last_mouse: None,
            last_click: None,
            pointer: Pointer::Default,
            notice: None,
            quit: false,
            output_tx,
            output_rx,
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

    /// Attach a freshly spawned command to an existing model pane and start
    /// its reader thread.
    fn attach(&mut self, id: PaneId, command: &str) -> Result<(), String> {
        let pty = Pty::spawn(command, SPAWN_COLS, SPAWN_ROWS).map_err(|e| e.to_string())?;
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
                pty,
                screen: Screen::new(SPAWN_COLS, SPAWN_ROWS),
                tracker: PaneTracker::new(),
                kind: self.detector.identify(command),
                generation,
                exited: None,
            },
        );
        if let Some(pane) = self.session.pane_mut(id) {
            pane.command = Some(command.to_string());
        }
        Ok(())
    }

    /// Drive the loop until quit or every pane is gone.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let started = Instant::now();
        while !self.quit && !self.session.is_empty() {
            self.drain_output();
            let size = terminal.size()?;
            self.last_area = Rect::new(0, 0, size.width, size.height);
            self.sync_layout(self.last_area);
            self.detect_if_due();

            self.last_entries = sidebar_entries(&self.session, &self.detector, Instant::now());
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
                Mode::Jump => self.sidebar.selected(self.last_entries.len()),
                _ => None,
            };
            // Hover follows the last known pointer position; the launcher
            // modal owns hover itself while open.
            let hover = match self.mode {
                Mode::Launch(_) => None,
                _ => self.last_mouse.map(|(x, y)| {
                    hit_test(
                        self.last_area,
                        &self.session,
                        self.side,
                        &self.last_entries,
                        self.zoomed_pane(),
                        x,
                        y,
                    )
                }),
            };
            let (mode_badge, status) = self.status_line();
            let launcher = match &self.mode {
                Mode::Launch(state) => Some((self.launchables.as_slice(), state)),
                _ => None,
            };
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
            }
        }
    }

    /// The pane's process ended: keep its final screen on display with an
    /// exited notice, and stop treating it as a live agent.
    fn mark_exited(&mut self, id: PaneId) {
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.exited.is_some() {
            return;
        }
        let code = match rt.pty.try_wait() {
            Ok(Some(status)) => status.code,
            _ => 1,
        };
        rt.exited = Some(code);
        rt.kind = None;
        self.session.set_reading(
            id,
            AgentState::Idle,
            Some(format!("exited ({code})")),
            Instant::now(),
        );
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
                let _ = rt.pty.resize(content.width, content.height);
            }
        }
    }

    fn detect_if_due(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_detect) < DETECT_EVERY {
            return;
        }
        self.last_detect = now;
        for (id, rt) in &mut self.runtimes {
            let Some(kind) = rt.kind else {
                continue;
            };
            let grid = rt.screen.grid();
            let reading = rt.tracker.update(&self.detector, kind, &grid, now);
            self.session
                .set_reading(*id, reading.state, reading.reason, now);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        self.notice = None;
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
                    KeyCode::Char('z') => self.zoomed = !self.zoomed,
                    KeyCode::Char('x') => {
                        if let Some(id) = self.session.focused() {
                            self.close_pane(id);
                        }
                    }
                    KeyCode::Char('j') => {
                        self.sidebar = SidebarState::new();
                        self.mode = Mode::Jump;
                    }
                    KeyCode::Char('q') => self.quit = true,
                    KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.write_to_focused(&[0x02]);
                    }
                    _ => {}
                }
            }
            Mode::Jump => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.sidebar.select_next(self.last_entries.len());
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sidebar.select_prev(self.last_entries.len());
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

        // The launcher owns the mouse while open: hovering a row selects
        // it, clicking launches it.
        if let Mode::Launch(state) = &mut self.mode {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let launcher = Launcher::new(&self.launchables, state);
                    if let Some(index) = launcher.item_at(self.last_area, x, y) {
                        state.select(index);
                        let command = state.command(&self.launchables);
                        self.mode = Mode::Normal;
                        if let Some(command) = command {
                            self.launch(&command);
                        }
                    } else if !launcher.contains(self.last_area, x, y) {
                        self.mode = Mode::Normal;
                    }
                }
                MouseEventKind::Moved => {
                    let launcher = Launcher::new(&self.launchables, state);
                    let item = launcher.item_at(self.last_area, x, y);
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
                self.notice = None;
                let hit = hit_test(
                    self.last_area,
                    &self.session,
                    self.side,
                    &self.last_entries,
                    self.zoomed_pane(),
                    x,
                    y,
                );
                match hit {
                    Hit::SidebarEntry(index) => {
                        if let Some(entry) = self.last_entries.get(index) {
                            self.session.focus(entry.pane);
                        }
                    }
                    Hit::SidebarNewAgent => {
                        self.mode = Mode::Launch(LauncherState::new());
                    }
                    Hit::SidebarViewGrid => self.zoomed = false,
                    Hit::SidebarViewSolo => self.zoomed = true,
                    Hit::PaneClose(id) => self.close_pane(id),
                    Hit::PaneTitle(id) | Hit::Pane(id) => {
                        self.session.focus(id);
                        // Double-clicking a title toggles solo, like
                        // double-clicking a window's title bar maximizes.
                        let double = self.last_click.is_some_and(|(at, pos)| {
                            at.elapsed() < Duration::from_millis(400) && pos == (x, y)
                        });
                        if double && matches!(hit, Hit::PaneTitle(_)) {
                            self.zoomed = !self.zoomed;
                        }
                        // Title rows and separator columns double as split
                        // dividers; grab one if it's there. Solo view has
                        // no dividers.
                        let panes = panes_area(self.last_area, self.side);
                        if !self.zoomed && x >= panes.x && y >= panes.y {
                            let local = (x - panes.x, y - panes.y);
                            if self
                                .session
                                .divider_at(panes.width, panes.height, local.0, local.1)
                                .is_some()
                            {
                                self.dragging = Some(local);
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
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = None,
            MouseEventKind::Moved => {
                let shape = self.pointer_shape_at(x, y);
                set_pointer(&mut self.pointer, shape);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let hit = hit_test(
                    self.last_area,
                    &self.session,
                    self.side,
                    &self.last_entries,
                    self.zoomed_pane(),
                    x,
                    y,
                );
                if let Hit::Pane(id) | Hit::PaneTitle(id) | Hit::PaneClose(id) = hit {
                    let bytes: &[u8] = if mouse.kind == MouseEventKind::ScrollUp {
                        b"\x1b[A\x1b[A\x1b[A"
                    } else {
                        b"\x1b[B\x1b[B\x1b[B"
                    };
                    if let Some(rt) = self.runtimes.get_mut(&id) {
                        if rt.exited.is_none() {
                            let _ = rt.pty.write(bytes);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The pointer shape for the position: a hand over anything clickable,
    /// resize arrows over a draggable divider, an I-beam over terminal
    /// content. Buttons win over dividers — a ✕ that shares a divider row
    /// still reads as a button.
    fn pointer_shape_at(&self, x: u16, y: u16) -> Pointer {
        let hit = hit_test(
            self.last_area,
            &self.session,
            self.side,
            &self.last_entries,
            self.zoomed_pane(),
            x,
            y,
        );
        if matches!(
            hit,
            Hit::PaneClose(_)
                | Hit::SidebarNewAgent
                | Hit::SidebarViewGrid
                | Hit::SidebarViewSolo
                | Hit::SidebarEntry(_)
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

    /// Start `command` in a new pane: split the focused pane along its
    /// longer visual axis (cells are roughly twice as tall as wide), or
    /// open a fresh window when nothing is left to split. A bare start's
    /// untouched placeholder shell is replaced outright — the user asked
    /// for an agent, not an extra terminal.
    fn launch(&mut self, command: &str) {
        if let Some(id) = self.placeholder.take() {
            if self.session.focused() == Some(id) {
                // Spawn first: a failed launch keeps the shell running.
                let old = self.runtimes.remove(&id);
                match self.attach(id, command) {
                    Ok(()) => {} // dropping the old runtime kills the shell
                    Err(error) => {
                        if let Some(rt) = old {
                            self.runtimes.insert(id, rt);
                        }
                        self.notice = Some(format!("launch failed: {error}"));
                    }
                }
                return;
            }
        }
        let id = match self.session.focused() {
            Some(target) => {
                let direction = match self.runtimes.get(&target) {
                    Some(rt) => {
                        let (cols, rows) = rt.screen.size();
                        if cols > rows * 2 {
                            SplitDirection::Horizontal
                        } else {
                            SplitDirection::Vertical
                        }
                    }
                    None => SplitDirection::Horizontal,
                };
                match self.session.split(target, direction) {
                    Some(id) => id,
                    None => return,
                }
            }
            None => self.session.new_window(),
        };
        if let Err(error) = self.attach(id, command) {
            self.session.close(id);
            self.notice = Some(format!("launch failed: {error}"));
        }
    }

    /// Split the focused pane and run a fresh shell in the new half.
    fn split(&mut self, direction: SplitDirection) {
        let Some(target) = self.session.focused() else {
            return;
        };
        let Some(id) = self.session.split(target, direction) else {
            return;
        };
        let shell = default_shell();
        if self.attach(id, &shell).is_err() {
            self.session.close(id);
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        if self.placeholder == Some(id) {
            self.placeholder = None;
        }
        // Dropping the runtime kills and reaps the child.
        self.runtimes.remove(&id);
        self.session.close(id);
    }

    fn write_to_focused(&mut self, bytes: &[u8]) {
        let Some(id) = self.session.focused() else {
            return;
        };
        // Typing into the placeholder shell claims it as a real pane.
        if self.placeholder == Some(id) {
            self.placeholder = None;
        }
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.exited.is_some() {
            return;
        }
        if rt.pty.write(bytes).is_err() {
            self.mark_exited(id);
        }
    }

    /// The status line: a badge while a mode is armed, plus contextual key
    /// hints — or the latest notice, until the next keypress clears it.
    fn status_line(&self) -> (Option<&'static str>, String) {
        if let Some(notice) = &self.notice {
            return (Some("!"), notice.clone());
        }
        match &self.mode {
            Mode::Normal => {
                let focused = self
                    .session
                    .focused()
                    .and_then(|id| self.session.pane(id))
                    .and_then(|pane| pane.command.as_deref())
                    .unwrap_or("");
                if self.zoomed {
                    (
                        Some("SOLO"),
                        format!(
                            "{focused}   click agents on the left to switch · grid tiles them again · ctrl-b for keys"
                        ),
                    )
                } else {
                    (
                        None,
                        format!(
                            "{focused}   click a pane to focus · ✕ closes · drag borders to resize · ctrl-b for keys"
                        ),
                    )
                }
            }
            Mode::Prefix => (
                Some("PREFIX"),
                "c: new agent · z: solo · %/\": split shell · o: focus · j: jump · x: close · q: quit"
                    .to_string(),
            ),
            Mode::Jump => (
                Some("JUMP"),
                "j/k: move · enter: jump to pane · esc: cancel".to_string(),
            ),
            Mode::Launch(_) => (
                Some("LAUNCH"),
                "type to filter or any command · click or ↑/↓ + enter to launch · esc: cancel"
                    .to_string(),
            ),
        }
    }
}

fn is_prefix(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL)
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
