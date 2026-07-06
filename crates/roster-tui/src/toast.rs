//! Toast notifications: transient cards stacked in the frame's top-right
//! corner. Errors (launch failures) get a red border and a ✗; confirmations
//! a quiet accent ✓. The binary owns their lifetime — spawning, expiry, and
//! click-to-dismiss — this module only lays them out and draws them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::launcher::{fill, frame};
use crate::style::ACCENT;

/// How loud a toast is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastLevel {
    /// A quiet confirmation ("copied").
    Info,
    /// Something failed and the user should read it.
    Error,
}

const TOAST_HEIGHT: u16 = 3;
const MAX_WIDTH: u16 = 44;

/// The rects the current toasts occupy, top-right of `area`, newest first,
/// stacked downward. Toasts that would not fit vertically get a zero-height
/// rect (skipped by render, dead to clicks).
pub fn toast_rects(area: Rect, toasts: &[(&str, ToastLevel)]) -> Vec<Rect> {
    let mut y = area.y + 1;
    toasts
        .iter()
        .map(|(text, _)| {
            // ✗, spaces, and the border take 6 columns around the text.
            let width = (text.chars().count() as u16 + 6)
                .clamp(12, MAX_WIDTH)
                .min(area.width.saturating_sub(2));
            let x = area.x + area.width.saturating_sub(width + 1);
            let fits = y + TOAST_HEIGHT + 1 < area.y + area.height;
            let rect = Rect::new(x, y, width, if fits { TOAST_HEIGHT } else { 0 });
            if fits {
                y += TOAST_HEIGHT + 1;
            }
            rect
        })
        .collect()
}

/// Draw every toast into `area`.
pub fn draw_toasts(buf: &mut Buffer, area: Rect, toasts: &[(&str, ToastLevel)]) {
    for (rect, (text, level)) in toast_rects(area, toasts).iter().zip(toasts) {
        if rect.height == 0 || rect.width < 8 {
            continue;
        }
        fill(buf, *rect);
        frame(buf, *rect, "");
        // Re-tint the border by level: errors read as red.
        let border = match level {
            ToastLevel::Info => Style::default().fg(ACCENT),
            ToastLevel::Error => Style::default().fg(Color::Red),
        };
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                let edge = y == rect.y
                    || y == rect.y + rect.height - 1
                    || x == rect.x
                    || x == rect.x + rect.width - 1;
                if edge {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(border);
                    }
                }
            }
        }
        let (glyph, glyph_style) = match level {
            ToastLevel::Info => ("✓", Style::default().fg(ACCENT)),
            ToastLevel::Error => (
                "✗",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        };
        buf.set_string(rect.x + 2, rect.y + 1, glyph, glyph_style);
        buf.set_stringn(
            rect.x + 4,
            rect.y + 1,
            *text,
            usize::from(rect.width.saturating_sub(6)),
            Style::default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_stack_from_the_top_right() {
        let area = Rect::new(0, 0, 120, 30);
        let toasts = vec![
            ("launch failed: no such command", ToastLevel::Error),
            ("copied", ToastLevel::Info),
        ];
        let rects = toast_rects(area, &toasts);
        assert_eq!(rects.len(), 2);
        // Right-aligned with a one-column inset.
        assert_eq!(rects[0].x + rects[0].width, 119);
        assert_eq!(rects[1].x + rects[1].width, 119);
        // Stacked downward with a gap.
        assert!(rects[1].y > rects[0].y + rects[0].height);
        // Width scales with the text but stays clamped.
        assert!(rects[0].width > rects[1].width);
        assert!(rects[0].width <= MAX_WIDTH);
    }

    #[test]
    fn overflowing_toasts_collapse_to_zero_height() {
        let area = Rect::new(0, 0, 60, 8);
        let toasts = vec![
            ("one", ToastLevel::Info),
            ("two", ToastLevel::Info),
            ("three", ToastLevel::Info),
        ];
        let rects = toast_rects(area, &toasts);
        assert_eq!(rects[0].height, TOAST_HEIGHT);
        assert_eq!(rects[1].height, 0);
        assert_eq!(rects[2].height, 0);
    }

    #[test]
    fn errors_render_red_with_a_cross() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let toasts = vec![("launch failed: nope", ToastLevel::Error)];
        draw_toasts(&mut buf, area, &toasts);
        let rect = toast_rects(area, &toasts)[0];
        let row: String = (rect.x..rect.x + rect.width)
            .map(|x| buf.cell((x, rect.y + 1)).unwrap().symbol().to_string())
            .collect();
        assert!(row.contains("✗ launch failed: nope"), "row: {row}");
        assert_eq!(buf.cell((rect.x, rect.y)).unwrap().style().fg, Some(Color::Red));
    }
}
