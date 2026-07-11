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
/// The smallest dialog that still works: wide enough for the button row
/// (cancel + gap + close) to sit inside the borders — any narrower and a
/// centered close button would overflow the frame, catching clicks past
/// the dialog's edge. `confirm_rect` floors to this and `confirm_drawable`
/// refuses anything smaller, so the two can't drift apart.
const MIN_WIDTH: u16 = 25;
/// Border, title gap, message, gap, buttons — the five rows the layout
/// hardcodes.
const MIN_HEIGHT: u16 = 5;
/// Rows from the modal's top to the button row.
const BUTTONS_ROW: u16 = 4;
const CANCEL_LABEL: &str = "  cancel  ";
const CLOSE_LABEL: &str = "  close  ";
/// Columns between the two buttons.
const BUTTON_GAP: u16 = 3;

/// The centered rect the dialog occupies within `area`. The minimum
/// footprint can exceed a sliver frame, so the rect is clipped to `area` —
/// the private `confirm_drawable` predicate is how render and hit-testing
/// agree the result is big enough to exist.
pub fn confirm_rect(area: Rect) -> Rect {
    let width = WIDTH.min(area.width.saturating_sub(2)).max(MIN_WIDTH);
    let height = HEIGHT.min(area.height.saturating_sub(2)).max(MIN_HEIGHT);
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 3,
        width,
        height,
    )
    .intersection(area)
}

/// Whether a clipped dialog rect is big enough to draw at all. Render
/// bails when this is false, and the hit tests consult it too — an
/// invisible dialog must never own a click, or a phantom close button
/// kills the agent with no dialog on screen.
fn confirm_drawable(modal: Rect) -> bool {
    modal.width >= MIN_WIDTH && modal.height >= MIN_HEIGHT
}

/// Whether (`x`, `y`) falls inside the dialog. Always false when the
/// dialog is too small to draw, so any click dismisses it.
pub fn confirm_contains(area: Rect, x: u16, y: u16) -> bool {
    let modal = confirm_rect(area);
    confirm_drawable(modal)
        && x >= modal.x
        && x < modal.x + modal.width
        && y >= modal.y
        && y < modal.y + modal.height
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
    if !confirm_drawable(confirm_rect(area)) {
        return None;
    }
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
        if !confirm_drawable(modal) {
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

    #[test]
    fn sliver_frames_neither_draw_nor_hit() {
        // Frames too small for the dialog's minimum footprint: nothing is
        // drawn (buffer-equality catches style-only writes too) and no
        // click lands — a phantom close button here would kill the agent
        // with no dialog on screen. MIN_WIDTH is the narrowest area the
        // dialog accepts, so one column under it must refuse.
        for (w, h) in [(1u16, 1u16), (80, 3), (80, 4), (MIN_WIDTH - 1, 24)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            Confirm::new().render(area, &mut buf);
            assert_eq!(buf, Buffer::empty(area), "drawn at {w}x{h}");
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        confirm_button_at(area, x, y),
                        None,
                        "phantom button at ({x},{y}) in {w}x{h}"
                    );
                    assert!(
                        !confirm_contains(area, x, y),
                        "phantom contains at ({x},{y}) in {w}x{h}"
                    );
                }
            }
        }
    }

    #[test]
    fn buttons_stay_inside_the_dialog_at_the_minimum_width() {
        // At the narrowest drawable width the centered button row must sit
        // inside the borders — a close button overflowing the frame would
        // catch clicks past the dialog's edge and kill the agent on what
        // reads as a click-outside dismissal.
        let area = Rect::new(0, 0, MIN_WIDTH, 24);
        let modal = confirm_rect(area);
        assert!(modal.width >= MIN_WIDTH, "dialog should draw here");
        let (cancel, close) = button_spans(area);
        assert!(cancel.x > modal.x, "cancel overflows the left border");
        assert!(
            close.x + close.width < modal.x + modal.width,
            "close overflows the right border"
        );
    }
}
