//! The event loop: PTY output → emulator → detection → model → repaint,
//! plus key handling and the pane-switch side effects.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use roster_core::{PaneId, Session, SplitDirection};
use roster_detect::{AgentKind, Detector, PaneTracker};
use roster_pty::Pty;
use roster_term::Screen;
use roster_tui::{
    render, sidebar_entries, Message, SidebarEntry, SidebarState, View, SIDEBAR_WIDTH,
};

use crate::keys::encode_key;

/// How long to wait for input before running another frame.
const INPUT_POLL: Duration = Duration::from_millis(50);
/// How often detection re-reads each pane's screen.
const DETECT_EVERY: Duration = Duration::from_millis(400);
/// Size panes are spawned at before the first layout pass corrects them.
const SPAWN_COLS: u16 = 80;
const SPAWN_ROWS: u16 = 24;

/// Bytes or end-of-stream from a pane's reader thread.
enum Output {
    Bytes(PaneId, Vec<u8>),
    Eof(PaneId),
}

/// Input interpretation state.
enum Mode {
    /// Keys go to the focused pane; the prefix key arms a command.
    Normal,
    /// The next key is a roster command.
    Prefix,
    /// Arrow/vim keys drive the sidebar selection.
    Jump,
}

/// Everything one live pane owns besides its model entry.
struct PaneRuntime {
    pty: Pty,
    screen: Screen,
    tracker: PaneTracker,
    kind: Option<AgentKind>,
}

/// The running multiplexer.
pub struct App {
    session: Session,
    runtimes: HashMap<PaneId, PaneRuntime>,
    detector: Detector,
    sidebar: SidebarState,
    mode: Mode,
    last_entries: Vec<SidebarEntry>,
    last_detect: Instant,
    quit: bool,
    output_tx: Sender<Output>,
    output_rx: Receiver<Output>,
}

impl App {
    /// Spawn one pane per command and assemble the initial layout,
    /// alternating split directions for a usable mosaic.
    pub fn new(detector: Detector, commands: &[String]) -> Result<App, String> {
        let (output_tx, output_rx) = mpsc::channel();
        let mut app = App {
            session: Session::new(),
            runtimes: HashMap::new(),
            detector,
            sidebar: SidebarState::new(),
            mode: Mode::Normal,
            last_entries: Vec::new(),
            last_detect: Instant::now() - DETECT_EVERY,
            quit: false,
            output_tx,
            output_rx,
        };

        let first = app.session.focused().expect("new session has a pane");
        app.attach(first, &commands[0])
            .map_err(|e| format!("spawning `{}`: {e}", commands[0]))?;
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
        let tx = self.output_tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Output::Eof(id));
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Output::Bytes(id, buf[..n].to_vec())).is_err() {
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
            },
        );
        if let Some(pane) = self.session.pane_mut(id) {
            pane.command = Some(command.to_string());
        }
        Ok(())
    }

    /// Drive the loop until quit or every pane is gone.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.quit && !self.session.is_empty() {
            self.drain_output();
            let size = terminal.size()?;
            self.sync_layout(size.width, size.height);
            self.detect_if_due();

            self.last_entries =
                sidebar_entries(&self.session, &self.detector, Instant::now());
            let grids: HashMap<_, _> = self
                .runtimes
                .iter()
                .map(|(id, rt)| (*id, rt.screen.grid()))
                .collect();
            let selected = match self.mode {
                Mode::Jump => self.sidebar.selected(self.last_entries.len()),
                _ => None,
            };
            let view = View {
                session: &self.session,
                grids: &grids,
                entries: &self.last_entries,
                selected,
            };
            terminal.draw(|frame| render(frame, &view))?;

            if event::poll(INPUT_POLL)? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        self.handle_key(key);
                    }
                    // Resizes are picked up from terminal.size() next frame.
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn drain_output(&mut self) {
        while let Ok(message) = self.output_rx.try_recv() {
            match message {
                Output::Bytes(id, bytes) => {
                    if let Some(rt) = self.runtimes.get_mut(&id) {
                        rt.screen.advance(&bytes);
                    }
                }
                Output::Eof(id) => self.close_pane(id),
            }
        }
    }

    /// Bring every pane's PTY and emulator to its laid-out size.
    fn sync_layout(&mut self, width: u16, height: u16) {
        let sidebar_width = SIDEBAR_WIDTH.min(width / 2);
        for (id, rect) in self.session.layout(width - sidebar_width, height) {
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let Some(rt) = self.runtimes.get_mut(&id) else {
                continue;
            };
            if rt.screen.size() != (rect.width, rect.height) {
                rt.screen.resize(rect.width, rect.height);
                let _ = rt.pty.resize(rect.width, rect.height);
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
                    KeyCode::Char('%') => self.split(SplitDirection::Horizontal),
                    KeyCode::Char('"') => self.split(SplitDirection::Vertical),
                    KeyCode::Char('o') => self.session.focus_next(),
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
                    if let Some(Message::JumpToPane(id)) =
                        self.sidebar.activate(&self.last_entries)
                    {
                        self.session.focus(id);
                    }
                    self.mode = Mode::Normal;
                }
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                _ => {}
            },
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
        // Dropping the runtime kills and reaps the child.
        self.runtimes.remove(&id);
        self.session.close(id);
    }

    fn write_to_focused(&mut self, bytes: &[u8]) {
        let Some(id) = self.session.focused() else {
            return;
        };
        let Some(rt) = self.runtimes.get_mut(&id) else {
            return;
        };
        if rt.pty.write(bytes).is_err() {
            self.close_pane(id);
        }
    }
}

fn is_prefix(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// The user's shell, for panes roster opens itself.
pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}
