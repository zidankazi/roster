//! Blitting a pane's screen grid into a ratatui buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use roster_core::Grid;

use crate::style::cell_style;

/// Renders one pane's [`Grid`] into its rect, cell for cell.
///
/// Content larger than the area is clipped at the right and bottom edges;
/// smaller content leaves the rest of the area untouched. Scrolling,
/// borders, and focus chrome are composition concerns that live above this
/// widget.
pub struct PaneView<'a> {
    grid: &'a Grid,
    selection: Option<((u16, u16), (u16, u16))>,
}

impl<'a> PaneView<'a> {
    /// A view over `grid`.
    pub fn new(grid: &'a Grid) -> Self {
        PaneView {
            grid,
            selection: None,
        }
    }

    /// A linear text selection to highlight, as two `(col, row)` endpoints
    /// in grid coordinates (either order).
    pub fn selection(mut self, selection: Option<((u16, u16), (u16, u16))>) -> Self {
        self.selection = selection;
        self
    }

    /// Whether (`col`, `row`) falls inside the linear selection.
    fn selected(&self, col: usize, row: usize) -> bool {
        let Some((a, b)) = self.selection else {
            return false;
        };
        // Normalize to reading order: sort by row, then column.
        let (mut a, mut b) = ((a.1, a.0), (b.1, b.0));
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let point = (row as u16, col as u16);
        point >= a && point <= b
    }
}

impl Widget for PaneView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = self.grid.rows().min(usize::from(area.height));
        let cols = self.grid.cols().min(usize::from(area.width));
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = self.grid.cell(col, row) else {
                    continue;
                };
                let x = area.x + col as u16;
                let y = area.y + row as u16;
                if let Some(target) = buf.cell_mut((x, y)) {
                    target.set_char(cell.ch);
                    let mut style = cell.style;
                    if self.selected(col, row) {
                        // Selection reads as inverted cells, like any
                        // terminal.
                        style.reverse = !style.reverse;
                    }
                    target.set_style(cell_style(style));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use roster_core::{Cell, CellStyle};

    fn buffer_row(buf: &Buffer, y: u16) -> String {
        let area = *buf.area();
        (area.x..area.right())
            .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn blits_at_the_area_origin() {
        let grid = Grid::from_text("hello\nworld");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        PaneView::new(&grid).render(Rect::new(2, 1, 7, 2), &mut buf);
        assert_eq!(buffer_row(&buf, 0), "          ");
        assert_eq!(buffer_row(&buf, 1), "  hello   ");
        assert_eq!(buffer_row(&buf, 2), "  world   ");
    }

    #[test]
    fn clips_content_to_the_area() {
        let grid = Grid::from_text("0123456789\nabcdefghij\nKLMNOPQRST");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        PaneView::new(&grid).render(Rect::new(0, 0, 4, 2), &mut buf);
        assert_eq!(buffer_row(&buf, 0), "0123");
        assert_eq!(buffer_row(&buf, 1), "abcd");
    }

    #[test]
    fn carries_cell_styles_through() {
        let mut grid = Grid::new(2, 1);
        grid.set(
            0,
            0,
            Cell {
                ch: 'x',
                style: CellStyle {
                    bold: true,
                    ..CellStyle::default()
                },
            },
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        PaneView::new(&grid).render(Rect::new(0, 0, 2, 1), &mut buf);
        let cell = buf.cell((0, 0)).unwrap();
        assert_eq!(cell.symbol(), "x");
        assert!(cell.style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn ignores_area_outside_the_buffer() {
        let grid = Grid::from_text("xy");
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        PaneView::new(&grid).render(Rect::new(1, 0, 2, 1), &mut buf);
        assert_eq!(buffer_row(&buf, 0), " x");
    }

    #[test]
    fn selection_inverts_the_linear_span() {
        let grid = Grid::from_text("abcd\nefgh");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        // From (2,0) to (1,1): linear selection covers c, d, e, f.
        PaneView::new(&grid)
            .selection(Some(((2, 0), (1, 1))))
            .render(Rect::new(0, 0, 4, 2), &mut buf);
        let reversed = |x: u16, y: u16| {
            buf.cell((x, y))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(!reversed(1, 0), "b outside");
        assert!(reversed(2, 0), "c inside");
        assert!(reversed(3, 0), "d inside");
        assert!(reversed(0, 1), "e inside");
        assert!(reversed(1, 1), "f inside");
        assert!(!reversed(2, 1), "g outside");
    }
}
