//! The parsed screen grid: rows of styled cells plus a cursor.
//!
//! In the full pipeline this is produced by `roster-term` from the raw PTY
//! byte stream. It lives here so that detection and rendering — which only
//! ever *consume* a grid — stay free of any PTY or emulator dependency and
//! can be driven entirely from fixtures in tests. Wide-character and
//! escape-sequence handling belong to the emulator, not to this type.
//!
//! See `docs/01-crates.md`.

/// A terminal color, as reported by the emulator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// One of the 16 named ANSI colors (0–15).
    Ansi(u8),
    /// An indexed color from the 256-color palette.
    Indexed(u8),
    /// A 24-bit truecolor value.
    Rgb(u8, u8, u8),
}

/// Visual attributes of a single cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellStyle {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Bold weight.
    pub bold: bool,
    /// Dim / faint intensity.
    pub dim: bool,
    /// Italic slant.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Reverse video (fg/bg swapped).
    pub reverse: bool,
}

/// One character cell of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The glyph in this cell.
    pub ch: char,
    /// The cell's visual attributes.
    pub style: CellStyle,
}

impl Cell {
    /// A cell holding `ch` with default styling.
    pub fn new(ch: char) -> Self {
        Cell {
            ch,
            style: CellStyle::default(),
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::new(' ')
    }
}

/// The cursor position within a grid, in cell coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Zero-based column.
    pub col: usize,
    /// Zero-based row.
    pub row: usize,
    /// Whether the cursor is currently shown.
    pub visible: bool,
}

/// A fixed-size screen of cells, row-major, plus a cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    /// Cursor position and visibility.
    pub cursor: Cursor,
}

impl Grid {
    /// An empty grid of `cols` × `rows` cells (all spaces, default style).
    pub fn new(cols: usize, rows: usize) -> Self {
        Grid {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            cursor: Cursor::default(),
        }
    }

    /// Build a grid from plain text, one line per row.
    ///
    /// The grid is sized to the longest line and the line count; shorter
    /// lines are padded with blank cells. Blank interior and trailing lines
    /// are preserved, which is what makes text fixtures faithful to real
    /// screens where the prompt sits above empty rows.
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let rows = lines.len();
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut grid = Grid::new(cols, rows);
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                grid.set(col, row, Cell::new(ch));
            }
        }
        grid
    }

    /// Number of columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Number of rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The cell at (`col`, `row`), or `None` if out of bounds.
    pub fn cell(&self, col: usize, row: usize) -> Option<&Cell> {
        if col < self.cols && row < self.rows {
            self.cells.get(row * self.cols + col)
        } else {
            None
        }
    }

    /// Replace the cell at (`col`, `row`). Out-of-bounds writes are ignored.
    pub fn set(&mut self, col: usize, row: usize, cell: Cell) {
        if col < self.cols && row < self.rows {
            self.cells[row * self.cols + col] = cell;
        }
    }

    /// The text of one row with trailing whitespace removed, or `None` if
    /// `row` is out of bounds.
    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.rows {
            return None;
        }
        let start = row * self.cols;
        let text: String = self.cells[start..start + self.cols]
            .iter()
            .map(|c| c.ch)
            .collect();
        Some(text.trim_end().to_string())
    }

    /// All rows as trailing-trimmed text, top to bottom.
    pub fn lines(&self) -> Vec<String> {
        (0..self.rows)
            .map(|r| self.row_text(r).expect("row in range"))
            .collect()
    }

    /// The text of a linear (reading-order) selection from `start` to
    /// `end`, both `(col, row)` inclusive and in either order — the way a
    /// terminal's click-drag selects. Single-row selections take the cell
    /// span; multi-row ones take the first row from its column to the end,
    /// whole rows between, and the last row up to its column. Rows are
    /// trailing-trimmed and joined with newlines.
    pub fn linear_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (mut a, mut b) = (start, end);
        // Normalize to reading order: (col, row) sorts by row, then col.
        if (a.1, a.0) > (b.1, b.0) {
            std::mem::swap(&mut a, &mut b);
        }
        let clamp_row = |r: usize| r.min(self.rows.saturating_sub(1));
        let (a, b) = ((a.0, clamp_row(a.1)), (b.0, clamp_row(b.1)));
        let row_span = |row: usize, from: usize, to: usize| -> String {
            let text: String = (from..=to.min(self.cols.saturating_sub(1)))
                .filter_map(|col| self.cell(col, row))
                .map(|c| c.ch)
                .collect();
            text.trim_end().to_string()
        };
        if a.1 == b.1 {
            return row_span(a.1, a.0.min(b.0), a.0.max(b.0));
        }
        let mut out = vec![row_span(a.1, a.0, self.cols.saturating_sub(1))];
        for row in a.1 + 1..b.1 {
            out.push(row_span(row, 0, self.cols.saturating_sub(1)));
        }
        out.push(row_span(b.1, 0, b.0));
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_grid_is_blank() {
        let g = Grid::new(4, 2);
        assert_eq!(g.cols(), 4);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.cell(3, 1), Some(&Cell::default()));
        assert_eq!(g.lines(), vec!["", ""]);
    }

    #[test]
    fn from_text_preserves_layout() {
        let g = Grid::from_text("ab\n\nlonger line\n");
        assert_eq!(g.rows(), 3);
        assert_eq!(g.cols(), "longer line".len());
        assert_eq!(g.row_text(0).unwrap(), "ab");
        assert_eq!(g.row_text(1).unwrap(), "");
        assert_eq!(g.row_text(2).unwrap(), "longer line");
    }

    #[test]
    fn from_text_keeps_blank_trailing_rows() {
        let g = Grid::from_text("prompt >\n\n");
        assert_eq!(g.rows(), 2);
        assert_eq!(g.lines(), vec!["prompt >", ""]);
    }

    #[test]
    fn out_of_bounds_reads_are_none() {
        let g = Grid::new(2, 2);
        assert_eq!(g.cell(2, 0), None);
        assert_eq!(g.cell(0, 2), None);
        assert_eq!(g.row_text(2), None);
    }

    #[test]
    fn out_of_bounds_writes_are_ignored() {
        let mut g = Grid::new(2, 2);
        g.set(5, 5, Cell::new('x'));
        assert_eq!(g.lines(), vec!["", ""]);
    }

    #[test]
    fn set_and_read_back() {
        let mut g = Grid::new(3, 1);
        g.set(1, 0, Cell::new('x'));
        assert_eq!(g.cell(1, 0).unwrap().ch, 'x');
        assert_eq!(g.row_text(0).unwrap(), " x");
    }

    #[test]
    fn row_text_trims_trailing_whitespace_only() {
        let mut g = Grid::new(5, 1);
        g.set(1, 0, Cell::new('a'));
        assert_eq!(g.row_text(0).unwrap(), " a");
    }

    #[test]
    fn unicode_glyphs_round_trip() {
        let g = Grid::from_text("❯ done ✓");
        assert_eq!(g.row_text(0).unwrap(), "❯ done ✓");
    }

    #[test]
    fn linear_text_selects_within_one_row() {
        let g = Grid::from_text("hello world");
        assert_eq!(g.linear_text((6, 0), (10, 0)), "world");
        // Either direction selects the same span.
        assert_eq!(g.linear_text((10, 0), (6, 0)), "world");
        assert_eq!(g.linear_text((0, 0), (0, 0)), "h");
    }

    #[test]
    fn linear_text_spans_rows_in_reading_order() {
        let g = Grid::from_text("first line\nmiddle\nlast line");
        assert_eq!(g.linear_text((6, 0), (3, 2)), "line\nmiddle\nlast");
        // Dragging upward selects the same text.
        assert_eq!(g.linear_text((3, 2), (6, 0)), "line\nmiddle\nlast");
    }

    #[test]
    fn linear_text_clamps_out_of_range_points() {
        let g = Grid::from_text("ab\ncd");
        assert_eq!(g.linear_text((0, 0), (99, 99)), "ab\ncd");
        assert_eq!(g.linear_text((99, 0), (99, 0)), "");
    }
}
