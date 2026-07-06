//! Terminal emulation: raw PTY bytes in, a screen grid out.
//!
//! [`Screen`] wraps `alacritty_terminal`'s parser and grid — escape
//! sequences, scroll regions, the alternate screen buffer (what makes
//! full-screen TUIs render right) are all alacritty's battle-tested code.
//! This crate only feeds it bytes and reads back a plain
//! [`roster_core::Grid`] snapshot, which is the boundary that keeps
//! `roster-detect` and `roster-tui` free of any emulator dependency.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor};

use roster_core::{Cell, CellStyle, Color, Grid};

/// Captures the title the application sets via OSC 0/2 — agent CLIs
/// broadcast their current task through it. Everything else is dropped.
#[derive(Clone, Default)]
struct TitleSink(Arc<Mutex<Option<String>>>);

impl EventListener for TitleSink {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => *self.0.lock().expect("title lock") = Some(title),
            Event::ResetTitle => *self.0.lock().expect("title lock") = None,
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
    term: Term<TitleSink>,
    parser: Processor,
    size: Size,
    title: Arc<Mutex<Option<String>>>,
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
        let sink = TitleSink::default();
        let title = sink.0.clone();
        Screen {
            term: Term::new(config, &size, sink),
            parser: Processor::new(),
            size,
            title,
        }
    }

    /// The title the application last set via OSC 0/2 — agent CLIs put
    /// their current task here. `None` when unset or reset.
    pub fn title(&self) -> Option<String> {
        self.title.lock().expect("title lock").clone()
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
