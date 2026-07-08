//! The exited-pane overlay: a centered card over a dead pane's content with
//! real `restart` / `close` buttons, replacing squinting at a one-line strip.
//! Panes too small to host the card fall back to the strip; the binary owns
//! the restart and close side effects.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::launcher::{fill, frame};

const CARD_WIDTH: u16 = 30;
const CARD_HEIGHT: u16 = 5;
const RESTART_LABEL: &str = "  restart  ";
const CLOSE_LABEL: &str = "  close  ";
/// Columns between the two buttons.
const BUTTON_GAP: u16 = 3;

/// The overlay card's rect, centered in a pane's absolute `content` rect.
/// `None` when the pane is too small to host it — callers fall back to the
/// one-line strip.
pub fn exited_card_rect(content: Rect) -> Option<Rect> {
    (content.width >= CARD_WIDTH && content.height >= CARD_HEIGHT).then(|| {
        Rect::new(
            content.x + (content.width - CARD_WIDTH) / 2,
            content.y + (content.height - CARD_HEIGHT) / 2,
            CARD_WIDTH,
            CARD_HEIGHT,
        )
    })
}

/// The card's button spans: `(restart, close)`, in absolute coordinates.
pub fn exited_buttons(content: Rect) -> Option<(Rect, Rect)> {
    let card = exited_card_rect(content)?;
    let restart_w = RESTART_LABEL.chars().count() as u16;
    let close_w = CLOSE_LABEL.chars().count() as u16;
    let total = restart_w + BUTTON_GAP + close_w;
    let x = card.x + (card.width.saturating_sub(total)) / 2;
    let y = card.y + card.height - 2;
    Some((
        Rect::new(x, y, restart_w, 1),
        Rect::new(x + restart_w + BUTTON_GAP, y, close_w, 1),
    ))
}

/// Draw the overlay into `content`: the agent's name and exit code with the
/// two buttons. `hover_restart` / `hover_close` highlight the hovered
/// button. Returns false when the pane is too small (nothing drawn).
pub fn draw_exited(
    buf: &mut Buffer,
    content: Rect,
    name: &str,
    code: u32,
    hover_restart: bool,
    hover_close: bool,
) -> bool {
    let Some(card) = exited_card_rect(content) else {
        return false;
    };
    fill(buf, card);
    frame(buf, card, " exited ");

    // The exit code is the payload — truncate the name, never the code.
    let suffix = format!(" · exit {code}");
    let room = usize::from(card.width.saturating_sub(4)).saturating_sub(suffix.chars().count());
    let name: String = if name.chars().count() > room {
        let mut cut: String = name.chars().take(room.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        name.to_string()
    };
    let message = format!("{name}{suffix}");
    let msg_x = card.x + (card.width.saturating_sub(message.chars().count() as u16)) / 2;
    buf.set_stringn(
        msg_x.max(card.x + 2),
        card.y + 1,
        &message,
        usize::from(card.width.saturating_sub(4)),
        Style::default().add_modifier(Modifier::BOLD),
    );

    let Some((restart, close)) = exited_buttons(content) else {
        return true;
    };
    let mut restart_style = Style::default()
        .fg(crate::style::ACCENT)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD);
    if hover_restart {
        restart_style = restart_style.remove_modifier(Modifier::REVERSED);
    }
    let mut close_style = Style::default().add_modifier(Modifier::REVERSED);
    if hover_close {
        close_style = close_style
            .add_modifier(Modifier::BOLD)
            .fg(crate::style::danger());
    }
    buf.set_string(restart.x, restart.y, RESTART_LABEL, restart_style);
    buf.set_string(close.x, close.y, CLOSE_LABEL, close_style);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_centers_and_buttons_land_inside_it() {
        let content = Rect::new(10, 5, 60, 20);
        let card = exited_card_rect(content).unwrap();
        assert_eq!(card.width, CARD_WIDTH);
        assert_eq!(card.height, CARD_HEIGHT);
        assert!(card.x > content.x && card.x + card.width < content.x + content.width);

        let (restart, close) = exited_buttons(content).unwrap();
        assert_eq!(restart.y, card.y + card.height - 2);
        assert_eq!(restart.y, close.y);
        assert!(restart.x >= card.x && close.x + close.width <= card.x + card.width);
        assert!(
            restart.x + restart.width < close.x,
            "buttons must not touch"
        );
    }

    #[test]
    fn tiny_panes_get_no_card() {
        assert!(exited_card_rect(Rect::new(0, 0, 20, 20)).is_none());
        assert!(exited_card_rect(Rect::new(0, 0, 60, 4)).is_none());
        assert!(exited_buttons(Rect::new(0, 0, 20, 4)).is_none());
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
        assert!(!draw_exited(
            &mut buf,
            Rect::new(0, 0, 20, 4),
            "claude-code",
            0,
            false,
            false
        ));
    }

    #[test]
    fn long_names_truncate_but_the_exit_code_survives() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        assert!(draw_exited(
            &mut buf,
            area,
            "definitely-not-a-command-xyz",
            127,
            false,
            false
        ));
        let all: String = (0..24u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(all.contains("· exit 127"), "code cut off:\n{all}");
        assert!(all.contains('…'), "name should show truncation:\n{all}");
    }

    #[test]
    fn overlay_renders_name_code_and_buttons() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        assert!(draw_exited(&mut buf, area, "claude-code", 1, false, false));
        let all: String = (0..24u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(all.contains("exited"), "missing title:\n{all}");
        assert!(
            all.contains("claude-code · exit 1"),
            "missing message:\n{all}"
        );
        assert!(all.contains("restart"), "missing restart:\n{all}");
        assert!(all.contains("close"), "missing close:\n{all}");
    }
}
