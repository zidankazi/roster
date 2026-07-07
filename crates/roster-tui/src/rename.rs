//! The workspace-rename dialog: a small centered modal with an input line.
//! Opened from a header double-click or prefix-,; the binary owns the input
//! state and the actual rename.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::launcher::{fill, frame};
use crate::style::muted;

const WIDTH: u16 = 46;
const HEIGHT: u16 = 5;

/// The centered rect the dialog occupies within `area`.
pub fn rename_rect(area: Rect) -> Rect {
    let width = WIDTH.min(area.width.saturating_sub(2)).max(20);
    let height = HEIGHT.min(area.height.saturating_sub(2)).max(4);
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 3,
        width,
        height,
    )
}

/// Whether (`x`, `y`) falls inside the dialog.
pub fn rename_contains(area: Rect, x: u16, y: u16) -> bool {
    let modal = rename_rect(area);
    x >= modal.x && x < modal.x + modal.width && y >= modal.y && y < modal.y + modal.height
}

/// Where the terminal cursor belongs: the end of the typed input.
pub fn rename_cursor(area: Rect, input: &str) -> (u16, u16) {
    let modal = rename_rect(area);
    let len = 2 + input.chars().count() as u16;
    (
        (modal.x + 2 + len).min(modal.x + modal.width.saturating_sub(2)),
        modal.y + 1,
    )
}

/// Draw the dialog: a title naming the workspace, the input line, and the
/// clear/cancel hint.
pub fn draw_rename(buf: &mut Buffer, area: Rect, window: usize, input: &str) {
    let modal = rename_rect(area);
    if modal.width < 20 || modal.height < 4 {
        return;
    }
    fill(buf, modal);
    frame(buf, modal, &format!(" rename workspace {} ", window + 1));
    let inner_w = usize::from(modal.width.saturating_sub(4));
    buf.set_stringn(
        modal.x + 2,
        modal.y + 1,
        format!("❯ {input}"),
        inner_w,
        Style::default().add_modifier(Modifier::BOLD),
    );
    buf.set_stringn(
        modal.x + 2,
        modal.y + modal.height.saturating_sub(2),
        "enter: save · empty input restores auto · esc: cancel",
        inner_w,
        muted(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_renders_title_input_and_hint() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        draw_rename(&mut buf, area, 1, "fix auth bug");
        let all: String = (0..24u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(all.contains("rename workspace 2"), "title:\n{all}");
        assert!(all.contains("❯ fix auth bug"), "input:\n{all}");
        assert!(all.contains("restores auto"), "hint:\n{all}");

        let modal = rename_rect(area);
        assert!(rename_contains(area, modal.x + 1, modal.y + 1));
        assert!(!rename_contains(area, modal.x.saturating_sub(1), modal.y));
        let (cx, cy) = rename_cursor(area, "ab");
        assert_eq!(cy, modal.y + 1);
        assert!(cx > modal.x + 2);
    }
}
