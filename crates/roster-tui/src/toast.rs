//! Toast notifications: transient cards stacked in the frame's top-right
//! corner. Errors (launch failures) get a red border and a ✗; warnings
//! (a rate limit filling up) the working yellow and a !; confirmations a
//! quiet accent ✓. The binary owns their lifetime — spawning, expiry, and
//! click-to-dismiss — this module only lays them out and draws them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::chrome_area;
use crate::launcher::{fill, frame};
use crate::style::{danger, normal, state_color, ACCENT, SURFACE_RAISED};
use roster_core::AgentState;

/// How loud a toast is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastLevel {
    /// A quiet confirmation ("copied").
    Info,
    /// A caution the user should notice soon (a rate-limit window past its
    /// warn threshold) — added because both existing glyphs mislead here:
    /// Info's ✓ says something succeeded, Error's ✗ says something failed,
    /// and a filling limit is neither.
    Warn,
    /// Something failed and the user should read it.
    Error,
}

const TOAST_HEIGHT: u16 = 3;
const MAX_WIDTH: u16 = 44;

/// The rects the current toasts occupy, top-right of the chrome (`area` is
/// the raw frame; the inset applies here like everywhere else), newest
/// first, stacked downward. Toasts that would not fit vertically get a
/// zero-height rect (skipped by render, dead to clicks).
pub fn toast_rects(area: Rect, toasts: &[(&str, ToastLevel)]) -> Vec<Rect> {
    let area = chrome_area(area);
    let mut y = area.y + 1;
    toasts
        .iter()
        .map(|(text, _)| {
            // ✗, spaces, and the border take 6 columns around the text,
            // which is measured in display cells — launch-failure toasts
            // quote the user's command, and wide chars cost two.
            let cells = Span::raw(*text).width().min(usize::from(MAX_WIDTH)) as u16;
            let width = (cells + 6)
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
        fill(buf, *rect, SURFACE_RAISED);
        frame(buf, *rect, "");
        // Re-tint the border by level: errors read as red, warnings as the
        // working yellow — the same escalation hues the sidebar badges use.
        let border = match level {
            ToastLevel::Info => Style::default().fg(ACCENT),
            ToastLevel::Warn => Style::default().fg(state_color(AgentState::Working)),
            ToastLevel::Error => Style::default().fg(danger()),
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
            ToastLevel::Warn => (
                "!",
                Style::default()
                    .fg(state_color(AgentState::Working))
                    .add_modifier(Modifier::BOLD),
            ),
            ToastLevel::Error => (
                "✗",
                Style::default().fg(danger()).add_modifier(Modifier::BOLD),
            ),
        };
        buf.set_string(rect.x + 2, rect.y + 1, glyph, glyph_style);
        buf.set_stringn(
            rect.x + 4,
            rect.y + 1,
            *text,
            usize::from(rect.width.saturating_sub(6)),
            normal(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_stack_from_the_top_right_of_the_chrome() {
        let area = Rect::new(0, 0, 120, 30);
        let toasts = vec![
            ("launch failed: no such command", ToastLevel::Error),
            ("copied", ToastLevel::Info),
        ];
        let rects = toast_rects(area, &toasts);
        assert_eq!(rects.len(), 2);
        // Right-aligned one column in from the chrome's right edge (118),
        // never jutting into the inset margin.
        assert_eq!(rects[0].x + rects[0].width, 117);
        assert_eq!(rects[1].x + rects[1].width, 117);
        assert!(rects[0].y > 0, "toasts start inside the inset");
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
    fn wide_char_toasts_size_by_cells() {
        // Ten double-width chars are twenty cells; sized by chars the card
        // would be sixteen wide and clip the message's tail.
        let area = Rect::new(0, 0, 120, 30);
        let toasts = vec![("启动失败：命令不存在", ToastLevel::Error)];
        let rect = toast_rects(area, &toasts)[0];
        assert_eq!(rect.width, 26);
        let mut buf = Buffer::empty(area);
        draw_toasts(&mut buf, area, &toasts);
        let row: String = (rect.x..rect.x + rect.width)
            .map(|x| buf.cell((x, rect.y + 1)).unwrap().symbol().to_string())
            .collect();
        assert!(row.contains('在'), "message tail clipped: {row}");
    }

    #[test]
    fn warnings_render_the_working_yellow_with_a_bang() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let toasts = vec![("5-hour limit at 71% · resets 2h", ToastLevel::Warn)];
        draw_toasts(&mut buf, area, &toasts);
        let rect = toast_rects(area, &toasts)[0];
        let row: String = (rect.x..rect.x + rect.width)
            .map(|x| buf.cell((x, rect.y + 1)).unwrap().symbol().to_string())
            .collect();
        assert!(row.contains("! 5-hour limit at 71%"), "row: {row}");
        // Border and glyph take the escalation yellow — the same hue the
        // sidebar's warn badges use — never the error red or the quiet ✓.
        let yellow = Some(state_color(AgentState::Working));
        assert_eq!(buf.cell((rect.x, rect.y)).unwrap().style().fg, yellow);
        let glyph = buf.cell((rect.x + 2, rect.y + 1)).unwrap().style();
        assert_eq!(glyph.fg, yellow);
        assert!(glyph.add_modifier.contains(Modifier::BOLD));
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
        assert_eq!(
            buf.cell((rect.x, rect.y)).unwrap().style().fg,
            Some(danger())
        );
        // The toast is a raised surface with its text on the normal tier.
        assert_eq!(
            buf.cell((rect.x + 1, rect.y + 1)).unwrap().style().bg,
            Some(SURFACE_RAISED)
        );
        assert_eq!(
            buf.cell((rect.x + 4, rect.y + 1)).unwrap().style().fg,
            normal().fg
        );
    }
}
