//! The close-confirmation dialog: a centered modal guarding live agents.
//!
//! Closing a pane whose agent is still running raises this instead of
//! killing it outright — same visual language as the launcher modal, with
//! real clickable buttons. The binary owns the actual close.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::launcher::{fill, frame};
use crate::style::{bright, SURFACE_RAISED};

/// The dialog's two click targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmButton {
    /// Keep the agent running.
    Cancel,
    /// Close the pane, killing the agent.
    Close,
}

const WIDTH: u16 = 46;
const HEIGHT: u16 = 7;
/// Rows from the modal's top to the button row.
const BUTTONS_ROW: u16 = 4;
const CANCEL_LABEL: &str = "  cancel  ";
const CLOSE_LABEL: &str = "  close  ";
/// Columns between the two buttons.
const BUTTON_GAP: u16 = 3;

/// The centered rect the dialog occupies within `area`.
pub fn confirm_rect(area: Rect) -> Rect {
    let width = WIDTH.min(area.width.saturating_sub(2)).max(20);
    let height = HEIGHT.min(area.height.saturating_sub(2)).max(5);
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 3,
        width,
        height,
    )
}

/// Whether (`x`, `y`) falls inside the dialog.
pub fn confirm_contains(area: Rect, x: u16, y: u16) -> bool {
    let modal = confirm_rect(area);
    x >= modal.x && x < modal.x + modal.width && y >= modal.y && y < modal.y + modal.height
}

/// The button spans on their row: `(cancel, close)`, in absolute columns.
fn button_spans(area: Rect) -> (Rect, Rect) {
    let modal = confirm_rect(area);
    let cancel_w = CANCEL_LABEL.chars().count() as u16;
    let close_w = CLOSE_LABEL.chars().count() as u16;
    let total = cancel_w + BUTTON_GAP + close_w;
    let x = modal.x + (modal.width.saturating_sub(total)) / 2;
    let y = modal.y + BUTTONS_ROW.min(modal.height.saturating_sub(2));
    (
        Rect::new(x, y, cancel_w, 1),
        Rect::new(x + cancel_w + BUTTON_GAP, y, close_w, 1),
    )
}

/// The button under (`x`, `y`), when one is there.
pub fn confirm_button_at(area: Rect, x: u16, y: u16) -> Option<ConfirmButton> {
    let (cancel, close) = button_spans(area);
    let inside = |r: Rect| x >= r.x && x < r.x + r.width && y == r.y;
    if inside(cancel) {
        Some(ConfirmButton::Cancel)
    } else if inside(close) {
        Some(ConfirmButton::Close)
    } else {
        None
    }
}

/// The dialog widget: a fixed confirmation message and cancel/close buttons.
#[derive(Default)]
pub struct Confirm {
    hover: Option<ConfirmButton>,
}

impl Confirm {
    /// A fresh close-confirmation dialog with no button hovered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Which button the pointer is over, for hover highlighting.
    pub fn hover(mut self, button: Option<ConfirmButton>) -> Self {
        self.hover = button;
        self
    }
}

impl Widget for Confirm {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let modal = confirm_rect(area);
        if modal.width < 20 || modal.height < 5 {
            return;
        }
        fill(buf, modal, SURFACE_RAISED);
        frame(buf, modal, " close agent? ");

        let message = "This ends the session. There's no undo.";
        let msg_x = modal.x + (modal.width.saturating_sub(message.chars().count() as u16)) / 2;
        buf.set_stringn(
            msg_x.max(modal.x + 2),
            modal.y + 2,
            message,
            usize::from(modal.width.saturating_sub(4)),
            bright().add_modifier(Modifier::BOLD),
        );

        let (cancel, close) = button_spans(area);
        // The quiet button pins its foreground so the reversal has a
        // defined light side on the raised surface.
        let mut cancel_style = bright().add_modifier(Modifier::REVERSED);
        if self.hover == Some(ConfirmButton::Cancel) {
            cancel_style = cancel_style.add_modifier(Modifier::BOLD);
        }
        let mut close_style = Style::default()
            .fg(crate::style::danger())
            .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        if self.hover == Some(ConfirmButton::Close) {
            close_style = close_style.remove_modifier(Modifier::REVERSED);
        }
        buf.set_string(cancel.x, cancel.y, CANCEL_LABEL, cancel_style);
        buf.set_string(close.x, close.y, CLOSE_LABEL, close_style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_renders_message_and_buttons() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        Confirm::new().render(area, &mut buf);
        let all: String = (0..24u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(all.contains("close agent?"), "missing title:\n{all}");
        assert!(
            all.contains("This ends the session. There's no undo."),
            "missing message:\n{all}"
        );
        assert!(all.contains("cancel"), "missing cancel button:\n{all}");
        assert!(all.contains("close"), "missing close button:\n{all}");

        // The dialog is a raised surface, its message on the bright tier —
        // the same system as every other piece of chrome.
        let modal = confirm_rect(area);
        for (x, y) in [(modal.x, modal.y), (modal.x + 3, modal.y + 2)] {
            assert_eq!(
                buf.cell((x, y)).unwrap().style().bg,
                Some(SURFACE_RAISED),
                "cell ({x},{y})"
            );
        }
        let msg_col = (0..80u16)
            .find(|x| buf.cell((*x, modal.y + 2)).unwrap().symbol() == "T")
            .expect("message row");
        assert_eq!(
            buf.cell((msg_col, modal.y + 2)).unwrap().style().fg,
            bright().fg
        );
    }

    #[test]
    fn buttons_hit_test_and_miss_elsewhere() {
        let area = Rect::new(0, 0, 80, 24);
        let (cancel, close) = button_spans(area);
        assert_eq!(
            confirm_button_at(area, cancel.x + 1, cancel.y),
            Some(ConfirmButton::Cancel)
        );
        assert_eq!(
            confirm_button_at(area, close.x + 1, close.y),
            Some(ConfirmButton::Close)
        );
        // The gap between them is dead space; the message row is too.
        assert_eq!(
            confirm_button_at(area, cancel.x + cancel.width + 1, cancel.y),
            None
        );
        assert_eq!(confirm_button_at(area, cancel.x, cancel.y - 2), None);
        // Inside the modal but not a button.
        let modal = confirm_rect(area);
        assert!(confirm_contains(area, modal.x + 1, modal.y + 1));
        assert!(!confirm_contains(area, modal.x.saturating_sub(1), modal.y));
    }
}
