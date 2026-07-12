//! Terminal emulation: raw PTY bytes in, a screen grid out.
//!
//! [`Screen`] wraps `alacritty_terminal`'s parser and grid — escape
//! sequences, scroll regions, the alternate screen buffer (what makes
//! full-screen TUIs render right) are all alacritty's battle-tested code.
//! This crate feeds it bytes and reads back a plain
//! [`roster_core::Grid`] snapshot of the viewport — the boundary that
//! keeps `roster-detect` and `roster-tui` free of any emulator dependency
//! — plus, for the binary's copy path only, linear selection text that may
//! span scrollback history ([`Screen::linear_text`]).

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor};

use roster_core::{Cell, CellStyle, Color, Grid};

/// Captures the guest-visible side effects the emulator reports: the title
/// set via OSC 0/2 (agent CLIs broadcast their current task through it) and
/// clipboard writes via OSC 52 (mouse-native guests copy their own
/// selections — the host must relay them or the copy silently vanishes).
/// Everything else is dropped; clipboard *reads* stay unanswered on purpose,
/// a guest must not see the host clipboard.
#[derive(Clone, Default)]
struct EventSink {
    title: Arc<Mutex<Option<String>>>,
    clipboard: Arc<Mutex<Vec<String>>>,
}

/// Most clipboard writes kept queued between drains. A guest looping OSC 52
/// must not grow host memory; the oldest writes give way — the newest is
/// what a real clipboard would end up holding anyway.
const CLIPBOARD_QUEUE_CAP: usize = 8;

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => *self.title.lock().expect("title lock") = Some(title),
            Event::ResetTitle => *self.title.lock().expect("title lock") = None,
            Event::ClipboardStore(_, text) => {
                let mut queue = self.clipboard.lock().expect("clipboard lock");
                if queue.len() == CLIPBOARD_QUEUE_CAP {
                    queue.remove(0);
                }
                queue.push(text);
            }
            _ => {}
        }
    }
}

/// A fixed viewport size, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    cols: u16,
    rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// One pane's emulated terminal: feed it the raw byte stream, read back the
/// current screen.
pub struct Screen {
    term: Term<EventSink>,
    parser: Processor,
    size: Size,
    title: Arc<Mutex<Option<String>>>,
    clipboard: Arc<Mutex<Vec<String>>>,
}

impl Screen {
    /// How many lines of history each pane keeps.
    const SCROLLBACK: usize = 10_000;

    /// An empty screen of `cols` × `rows` cells, with scrollback.
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = Size { cols, rows };
        let config = Config {
            scrolling_history: Screen::SCROLLBACK,
            ..Config::default()
        };
        let sink = EventSink::default();
        let title = sink.title.clone();
        let clipboard = sink.clipboard.clone();
        Screen {
            term: Term::new(config, &size, sink),
            parser: Processor::new(),
            size,
            title,
            clipboard,
        }
    }

    /// The title the application last set via OSC 0/2 — agent CLIs put
    /// their current task here. `None` when unset or reset.
    pub fn title(&self) -> Option<String> {
        self.title.lock().expect("title lock").clone()
    }

    /// Drain the clipboard payloads the guest wrote via OSC 52 since the
    /// last call, oldest first, already base64-decoded by the emulator. The
    /// host relays them to the real clipboard — a mouse-native guest (Claude
    /// Code drag-selects its own transcript) copies through this path.
    pub fn take_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.clipboard.lock().expect("clipboard lock"))
    }

    /// Feed raw bytes from the PTY into the emulator.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the viewport, reflowing content.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == (self.size.cols, self.size.rows) {
            return;
        }
        self.size = Size { cols, rows };
        self.term.resize(self.size);
    }

    /// Current viewport size as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (self.size.cols, self.size.rows)
    }

    /// Whether the application asked for bracketed paste (DECSET 2004).
    /// Pasted text should then be wrapped in `ESC[200~`/`ESC[201~` guards.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Whether the application switched to the alternate screen (a
    /// full-screen TUI). Such apps own their own history — wheel scrolling
    /// should feed them arrow keys, not move roster's scrollback.
    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the application asked to receive mouse events (any of the
    /// DECSET 1000/1002/1003 tracking modes). Such apps handle the wheel
    /// themselves — Claude Code scrolls its own transcript — so roster
    /// should forward the raw event, not translate it to arrow keys.
    pub fn mouse_reporting(&self) -> bool {
        self.term.mode().intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        )
    }

    /// Whether the application asked for drag/motion reports (DECSET
    /// 1002/1003). Click-only tracking (1000) alone must not receive
    /// synthetic button-motion reports it never subscribed to.
    pub fn mouse_drag_reporting(&self) -> bool {
        self.term
            .mode()
            .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    }

    /// Whether the application negotiated SGR mouse encoding (DECSET 1006).
    /// A forwarded mouse report must use the `CSI < … M` form only when this
    /// is set; a tracking app that left it off expects the legacy X10 byte
    /// encoding and would misread SGR bytes as stray keystrokes.
    pub fn sgr_mouse(&self) -> bool {
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    /// How far the view is scrolled up into history, in lines. Zero means
    /// live at the bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// How many lines of history are available to scroll into.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Scroll the view by `delta` lines: positive scrolls up into history,
    /// negative back toward live output. Clamped to the history extent.
    pub fn scroll_display(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    /// Jump back to live output.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// The text of a linear (reading-order) selection between two absolute
    /// points, `(col, row)` in either order, where rows count from the top
    /// of scrollback history: row 0 is the oldest kept line and row
    /// [`history_size()`](Self::history_size) is the top of the live
    /// viewport. Unlike [`Grid::linear_text`], the span may cross lines
    /// scrolled out of view. The semantics otherwise match: single-row
    /// selections take the cell span; multi-row ones take the first row
    /// from its column to the end, whole rows between, and the last row up
    /// to its column; rows and columns clamp to the buffer, rows are
    /// trailing-trimmed and joined with newlines.
    pub fn linear_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let grid = self.term.grid();
        let history = grid.history_size();
        let total = history + self.size.screen_lines();
        let cols = self.size.columns();
        if total == 0 || cols == 0 {
            return String::new();
        }
        let (mut a, mut b) = (start, end);
        // Normalize to reading order: (col, row) sorts by row, then col.
        if (a.1, a.0) > (b.1, b.0) {
            std::mem::swap(&mut a, &mut b);
        }
        let clamp_row = |r: usize| r.min(total - 1);
        let (a, b) = ((a.0, clamp_row(a.1)), (b.0, clamp_row(b.1)));
        let row_span = |row: usize, from: usize, to: usize| -> String {
            // Absolute rows sit above the viewport as negative Line values.
            let line = &grid[Line(row as i32 - history as i32)];
            let text: String = (from..=to.min(cols - 1))
                .map(|col| line[Column(col)].c)
                .collect();
            text.trim_end().to_string()
        };
        if a.1 == b.1 {
            return row_span(a.1, a.0.min(b.0), a.0.max(b.0));
        }
        let mut out = vec![row_span(a.1, a.0, cols - 1)];
        for row in a.1 + 1..b.1 {
            out.push(row_span(row, 0, cols - 1));
        }
        out.push(row_span(b.1, 0, b.0));
        out.join("\n")
    }

    /// Snapshot the displayed viewport as a plain [`Grid`] — the visible
    /// screen, or a window into history while scrolled up (cursor hidden).
    pub fn grid(&self) -> Grid {
        let cols = self.size.columns();
        let rows = self.size.screen_lines();
        let offset = self.term.grid().display_offset() as i32;
        let mut grid = Grid::new(cols, rows);
        let source = self.term.grid();
        for row in 0..rows {
            let line = &source[Line(row as i32 - offset)];
            for col in 0..cols {
                let cell = &line[Column(col)];
                grid.set(
                    col,
                    row,
                    Cell {
                        ch: cell.c,
                        style: convert_style(cell),
                    },
                );
            }
        }
        let cursor = source.cursor.point;
        grid.cursor.col = cursor.column.0;
        grid.cursor.row = cursor.line.0.max(0) as usize;
        grid.cursor.visible = offset == 0 && self.term.mode().contains(TermMode::SHOW_CURSOR);
        grid
    }
}

fn convert_style(cell: &alacritty_terminal::term::cell::Cell) -> CellStyle {
    CellStyle {
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        bold: cell.flags.contains(Flags::BOLD),
        dim: cell.flags.contains(Flags::DIM),
        italic: cell.flags.contains(Flags::ITALIC),
        underline: cell.flags.contains(Flags::UNDERLINE),
        reverse: cell.flags.contains(Flags::INVERSE),
    }
}

fn convert_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Named(named) => {
            let index = named as usize;
            if index <= NamedColor::BrightWhite as usize {
                Color::Ansi(index as u8)
            } else {
                // Foreground, Background, cursor, and dim variants all fall
                // back to the terminal default; roster's UI doesn't theme
                // them.
                Color::Default
            }
        }
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(index) if index <= 15 => Color::Ansi(index),
        AnsiColor::Indexed(index) => Color::Indexed(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_lands_on_the_grid() {
        let mut screen = Screen::new(20, 4);
        screen.advance(b"hello\r\nworld");
        let grid = screen.grid();
        assert_eq!(grid.row_text(0).unwrap(), "hello");
        assert_eq!(grid.row_text(1).unwrap(), "world");
        assert_eq!(grid.cols(), 20);
        assert_eq!(grid.rows(), 4);
    }

    #[test]
    fn carriage_return_overwrites_the_line() {
        let mut screen = Screen::new(20, 2);
        screen.advance(b"progress 10%\rprogress 99%");
        assert_eq!(screen.grid().row_text(0).unwrap(), "progress 99%");
    }

    #[test]
    fn osc52_clipboard_writes_surface_decoded_and_drain() {
        let mut screen = Screen::new(20, 4);
        assert!(screen.take_clipboard_writes().is_empty());
        // "hello" is aGVsbG8= in base64; two writes queue in order.
        screen.advance(b"\x1b]52;c;aGVsbG8=\x07");
        screen.advance(b"\x1b]52;c;d29ybGQ=\x07");
        assert_eq!(screen.take_clipboard_writes(), ["hello", "world"]);
        // Draining empties the queue.
        assert!(screen.take_clipboard_writes().is_empty());
    }

    #[test]
    fn corrupt_osc52_payloads_never_panic_or_surface() {
        let mut screen = Screen::new(20, 4);
        // Invalid base64 and a query ('?', a clipboard READ) must both be
        // dropped: the guest never learns the host clipboard.
        screen.advance(b"\x1b]52;c;!!!not-base64!!!\x07");
        screen.advance(b"\x1b]52;c;?\x07");
        assert!(screen.take_clipboard_writes().is_empty());
    }

    #[test]
    fn clipboard_queue_caps_and_keeps_the_newest_writes() {
        let mut screen = Screen::new(20, 4);
        // 4× "hello", 7× "world", then one last "hello" — 12 writes into a
        // cap of 8. The oldest give way: a guest looping OSC 52 can't grow
        // host memory, and the newest write (what a real clipboard would
        // hold) always survives.
        for _ in 0..4 {
            screen.advance(b"\x1b]52;c;aGVsbG8=\x07");
        }
        for _ in 0..7 {
            screen.advance(b"\x1b]52;c;d29ybGQ=\x07");
        }
        screen.advance(b"\x1b]52;c;aGVsbG8=\x07");
        let writes = screen.take_clipboard_writes();
        assert_eq!(writes.len(), 8);
        assert_eq!(writes.first().map(String::as_str), Some("world"));
        assert_eq!(writes.last().map(String::as_str), Some("hello"));
    }

    #[test]
    fn ansi_colors_and_attributes_convert() {
        let mut screen = Screen::new(20, 2);
        screen.advance(b"\x1b[1;31mrX");
        let grid = screen.grid();
        let cell = grid.cell(0, 0).unwrap();
        assert_eq!(cell.ch, 'r');
        assert_eq!(cell.style.fg, Color::Ansi(1));
        assert!(cell.style.bold);
    }

    #[test]
    fn palette_and_truecolor_convert() {
        let mut screen = Screen::new(20, 2);
        screen.advance(b"\x1b[38;5;196mA\x1b[38;2;10;20;30mB");
        let grid = screen.grid();
        assert_eq!(grid.cell(0, 0).unwrap().style.fg, Color::Indexed(196));
        assert_eq!(grid.cell(1, 0).unwrap().style.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn cursor_position_tracks_moves() {
        let mut screen = Screen::new(20, 5);
        screen.advance(b"\x1b[3;7H");
        let grid = screen.grid();
        assert_eq!((grid.cursor.row, grid.cursor.col), (2, 6));
        assert!(grid.cursor.visible);
    }

    #[test]
    fn hidden_cursor_is_reported() {
        let mut screen = Screen::new(20, 2);
        screen.advance(b"\x1b[?25l");
        assert!(!screen.grid().cursor.visible);
        screen.advance(b"\x1b[?25h");
        assert!(screen.grid().cursor.visible);
    }

    #[test]
    fn alternate_screen_switches_and_restores() {
        let mut screen = Screen::new(20, 3);
        screen.advance(b"shell history");
        screen.advance(b"\x1b[?1049h\x1b[2J\x1b[Hfullscreen tui");
        assert_eq!(screen.grid().row_text(0).unwrap(), "fullscreen tui");
        screen.advance(b"\x1b[?1049l");
        assert_eq!(screen.grid().row_text(0).unwrap(), "shell history");
    }

    #[test]
    fn clear_screen_blanks_the_grid() {
        let mut screen = Screen::new(20, 3);
        screen.advance(b"one\r\ntwo\r\nthree");
        screen.advance(b"\x1b[2J\x1b[H");
        assert_eq!(screen.grid().lines(), vec!["", "", ""]);
    }

    #[test]
    fn long_lines_wrap() {
        let mut screen = Screen::new(8, 3);
        screen.advance(b"0123456789ab");
        let grid = screen.grid();
        assert_eq!(grid.row_text(0).unwrap(), "01234567");
        assert_eq!(grid.row_text(1).unwrap(), "89ab");
    }

    #[test]
    fn unicode_survives_the_round_trip() {
        let mut screen = Screen::new(20, 2);
        screen.advance("❯ ⠹ done ✓".as_bytes());
        assert_eq!(screen.grid().row_text(0).unwrap(), "❯ ⠹ done ✓");
    }

    #[test]
    fn resize_preserves_content() {
        let mut screen = Screen::new(20, 4);
        screen.advance(b"keep me");
        screen.resize(30, 6);
        assert_eq!(screen.size(), (30, 6));
        let grid = screen.grid();
        assert_eq!(grid.cols(), 30);
        assert_eq!(grid.rows(), 6);
        assert_eq!(grid.row_text(0).unwrap(), "keep me");
    }

    #[test]
    fn scrollback_scrolls_into_history_and_back() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        // Live view shows the tail.
        assert_eq!(screen.display_offset(), 0);
        assert!(screen.grid().lines().join("\n").contains("line9"));

        screen.scroll_display(5);
        assert_eq!(screen.display_offset(), 5);
        let scrolled = screen.grid();
        assert!(
            scrolled.lines().join("\n").contains("line3"),
            "history view: {:?}",
            scrolled.lines()
        );
        // The cursor hides while looking at history.
        assert!(!scrolled.cursor.visible);

        screen.scroll_to_bottom();
        assert_eq!(screen.display_offset(), 0);
        assert!(screen.grid().cursor.visible);

        // Scrolling is clamped to what history holds.
        screen.scroll_display(100_000);
        assert!(screen.display_offset() <= 10);
    }

    #[test]
    fn new_output_keeps_a_scrolled_view_stable() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10 {
            screen.advance(format!("old{i}\r\n").as_bytes());
        }
        screen.scroll_display(4);
        let before = screen.grid().lines();
        screen.advance(b"new line\r\n");
        // The view is pinned to the same history lines, not dragged along.
        assert_eq!(screen.grid().lines(), before);
        assert!(screen.display_offset() > 4);
    }

    #[test]
    fn linear_text_spans_scrollback_history() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        // 11 buffer lines (ten printed plus the empty prompt row), three
        // visible: eight lines sit in history above the viewport.
        assert_eq!(screen.history_size(), 8);
        let expected = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>();
        assert_eq!(screen.linear_text((0, 0), (9, 9)), expected.join("\n"));
        // Either direction selects the same text.
        assert_eq!(screen.linear_text((9, 9), (0, 0)), expected.join("\n"));
        // A span wholly inside history never touches the viewport.
        assert_eq!(screen.linear_text((0, 2), (9, 3)), "line2\nline3");
    }

    #[test]
    fn absolute_selection_survives_scrolling_between_readings() {
        let mut screen = Screen::new(10, 3);
        for i in 0..20 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        // Anchor on the viewport's top row while live, the way the app
        // converts a pointer reading: buffer row = history - offset + row.
        let anchor = screen.history_size() - screen.display_offset();
        assert_eq!(screen.linear_text((0, anchor), (9, anchor)), "line18");
        // Scroll up; the same pointer position now reads five lines earlier,
        // and the span between the two readings covers the scrolled lines.
        screen.scroll_display(5);
        let end = screen.history_size() - screen.display_offset();
        assert_eq!(anchor - end, 5);
        let expected = (13..=18).map(|i| format!("line{i}")).collect::<Vec<_>>();
        assert_eq!(
            screen.linear_text((0, end), (9, anchor)),
            expected.join("\n")
        );
    }

    #[test]
    fn new_output_leaves_absolute_rows_pinned_to_their_text() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        let anchor = screen.history_size();
        assert_eq!(screen.linear_text((0, anchor), (9, anchor)), "line8");
        // More output pushes lines into history; the absolute row still
        // names the same text because both count from the history top.
        for i in 10..15 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(screen.linear_text((0, anchor), (9, anchor)), "line8");
    }

    #[test]
    fn linear_text_clamps_out_of_range_absolute_points() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        // Rows past the buffer clamp to the last (blank) viewport row,
        // columns past the width clamp to the width.
        let total = screen.history_size() + 3;
        assert_eq!(
            screen.linear_text((0, 9), (99, total + 50)),
            "line9\n",
            "clamped tail row is the trailing blank viewport row"
        );
        assert_eq!(screen.linear_text((99, 0), (99, 0)), "");
    }

    #[test]
    fn linear_text_on_the_alternate_screen_reads_the_viewport() {
        let mut screen = Screen::new(10, 3);
        screen.advance(b"shell\r\n");
        screen.advance(b"\x1b[?1049h\x1b[2J\x1b[Halpha\r\nbeta");
        // The alternate screen keeps no history, so absolute rows are
        // plain viewport rows — exactly today's visible-grid behavior.
        assert_eq!(screen.history_size(), 0);
        assert_eq!(screen.linear_text((0, 0), (9, 1)), "alpha\nbeta");
    }

    #[test]
    fn absolute_and_viewport_extraction_agree_on_the_live_screen() {
        let mut screen = Screen::new(12, 4);
        screen.advance(b"first line\r\nmiddle\r\nlast line");
        // With no history the two APIs share a coordinate space and must
        // return byte-identical text — the consistency contract.
        assert_eq!(screen.history_size(), 0);
        for (a, b) in [((6, 0), (3, 2)), ((0, 0), (11, 3)), ((4, 1), (4, 1))] {
            assert_eq!(screen.linear_text(a, b), screen.grid().linear_text(a, b));
        }
    }

    #[test]
    fn absolute_and_viewport_extraction_agree_while_scrolled() {
        // grid() returns a window into history while scrolled; extracting
        // the same span through viewport coordinates and through absolute
        // rows must agree — this is the guard against the two linear_text
        // implementations drifting apart on history reads.
        let mut screen = Screen::new(10, 3);
        for i in 0..20 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        screen.scroll_display(7);
        let top = screen.history_size() - screen.display_offset();
        for (a, b) in [((0, 0), (9, 2)), ((3, 0), (2, 1)), ((5, 2), (5, 2))] {
            assert_eq!(
                screen.linear_text((a.0, top + a.1), (b.0, top + b.1)),
                screen.grid().linear_text(a, b),
                "span {a:?}..{b:?} diverged"
            );
        }
    }

    #[test]
    fn history_trimming_clamps_but_never_panics() {
        let mut screen = Screen::new(10, 3);
        for i in 0..10_100 {
            screen.advance(format!("l{i}\r\n").as_bytes());
        }
        // History is capped: the oldest lines are gone and row 0 now names
        // the oldest *kept* line. A selection held across the trim drifts
        // by the trimmed amount — the clamp keeps it valid, not exact.
        assert_eq!(screen.history_size(), Screen::SCROLLBACK);
        let all = screen.linear_text((0, 0), (9, usize::MAX));
        // 10,101 lines ever (10,100 printed + the prompt row), 10,003 kept
        // (10,000 history + 3 viewport): the first 98 are gone.
        assert!(
            all.starts_with("l98\n"),
            "unexpected head: {:?}",
            &all[..12]
        );
        assert!(all.ends_with("l10099\n"), "unexpected tail");
    }

    #[test]
    fn alternate_screen_mode_is_reported() {
        let mut screen = Screen::new(10, 3);
        assert!(!screen.alternate_screen());
        screen.advance(b"\x1b[?1049h");
        assert!(screen.alternate_screen());
        screen.advance(b"\x1b[?1049l");
        assert!(!screen.alternate_screen());
    }

    #[test]
    fn osc_titles_are_captured_and_reset() {
        let mut screen = Screen::new(20, 3);
        assert_eq!(screen.title(), None);
        screen.advance(b"\x1b]0;fix auth bug\x07");
        assert_eq!(screen.title().as_deref(), Some("fix auth bug"));
        // OSC 2 (title only) works too, and BEL/ST terminators both parse.
        screen.advance(b"\x1b]2;compiling roster\x1b\\");
        assert_eq!(screen.title().as_deref(), Some("compiling roster"));
        // A reset clears it back to none.
        screen.advance(b"\x1b]0;\x07");
        assert_eq!(screen.title().as_deref(), Some(""));
    }

    #[test]
    fn mouse_reporting_tracks_the_tracking_modes() {
        let mut screen = Screen::new(10, 2);
        assert!(!screen.mouse_reporting());
        // Any tracking mode counts; Claude Code turns on click+drag+motion.
        screen.advance(b"\x1b[?1000h");
        assert!(screen.mouse_reporting());
        screen.advance(b"\x1b[?1000l");
        assert!(!screen.mouse_reporting());
        screen.advance(b"\x1b[?1002h");
        assert!(screen.mouse_reporting());
        screen.advance(b"\x1b[?1002l");
        screen.advance(b"\x1b[?1003h");
        assert!(screen.mouse_reporting());
        // The SGR-encoding toggle (1006) alone is not a tracking mode.
        screen.advance(b"\x1b[?1003l");
        screen.advance(b"\x1b[?1006h");
        assert!(!screen.mouse_reporting());
    }

    #[test]
    fn sgr_mouse_tracks_decset_1006() {
        let mut screen = Screen::new(10, 2);
        assert!(!screen.sgr_mouse());
        // Tracking without 1006 (legacy X10 encoding) is not SGR.
        screen.advance(b"\x1b[?1002h");
        assert!(!screen.sgr_mouse());
        screen.advance(b"\x1b[?1006h");
        assert!(screen.sgr_mouse());
        screen.advance(b"\x1b[?1006l");
        assert!(!screen.sgr_mouse());
    }

    #[test]
    fn bracketed_paste_mode_tracks_decset_2004() {
        let mut screen = Screen::new(10, 2);
        assert!(!screen.bracketed_paste());
        screen.advance(b"\x1b[?2004h");
        assert!(screen.bracketed_paste());
        screen.advance(b"\x1b[?2004l");
        assert!(!screen.bracketed_paste());
    }

    #[test]
    fn reverse_video_maps_to_reverse() {
        let mut screen = Screen::new(10, 1);
        screen.advance(b"\x1b[7mX");
        assert!(screen.grid().cell(0, 0).unwrap().style.reverse);
    }

    #[test]
    fn scrollback_survives_resizing_either_side_of_the_output() {
        // Resize after output — the shrink must not drop history.
        let mut screen = Screen::new(80, 24);
        for i in 0..200 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        screen.resize(68, 22);
        screen.scroll_display(90);
        assert!(screen.display_offset() > 0, "offset stuck after resize");

        // Resize before output — the app's order at startup.
        let mut screen = Screen::new(80, 24);
        screen.resize(68, 22);
        for i in 0..200 {
            screen.advance(format!("line{i}\r\n").as_bytes());
        }
        assert!(screen.history_size() > 0, "no history accumulated");
        screen.scroll_display(3);
        assert!(
            screen.display_offset() > 0,
            "offset stuck after early resize"
        );
    }
}
