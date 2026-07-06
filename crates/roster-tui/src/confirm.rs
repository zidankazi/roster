//! The close-confirmation dialog: a centered modal guarding live agents.
//!
//! Closing a pane whose agent is still running raises this instead of
//! killing it outright — same visual language as the launcher modal, with
//! real clickable buttons. The binary owns the actual close.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::launcher::{fill, frame};

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

/// The dialog widget: a message naming the agent, and cancel/close buttons.
pub struct Confirm<'a> {
    name: &'a str,
    hover: Option<ConfirmButton>,
}

impl<'a> Confirm<'a> {
    /// A dialog about closing the agent called `name`.
    pub fn new(name: &'a str) -> Self {
        Confirm { name, hover: None }
    }

    /// Which button the pointer is over, for hover highlighting.
    pub fn hover(mut self, button: Option<ConfirmButton>) -> Self {
        self.hover = button;
        self
    }
}

impl Widget for Confirm<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let modal = confirm_rect(area);
        if modal.width < 20 || modal.height < 5 {
            return;
        }
        fill(buf, modal);
        frame(buf, modal, " close agent? ");

        let message = format!("{} is still running.", self.name);
        let msg_x = modal.x + (modal.width.saturating_sub(message.chars().count() as u16)) / 2;
        buf.set_stringn(
            msg_x.max(modal.x + 2),
            modal.y + 2,
            &message,
            usize::from(modal.width.saturating_sub(4)),
            Style::default().add_modifier(Modifier::BOLD),
        );

        let (cancel, close) = button_spans(area);
        let mut cancel_style = Style::default().add_modifier(Modifier::REVERSED);
        if self.hover == Some(ConfirmButton::Cancel) {
            cancel_style = cancel_style.add_modifier(Modifier::BOLD);
        }
        let mut close_style = Style::default()
            .fg(Color::Red)
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
        Confirm::new("claude-code").render(area, &mut buf);
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
            all.contains("claude-code is still running."),
            "missing message:\n{all}"
        );
        assert!(all.contains("cancel"), "missing cancel button:\n{all}");
        assert!(all.contains("close"), "missing close button:\n{all}");
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
