//! ratatui rendering: pane contents and the agent-state sidebar.
//!
//! Renders a known model into a ratatui buffer — deterministic and
//! snapshot-testable. Input is surfaced as intent [`Message`]s; the binary
//! owns the side effects. See `docs/01-crates.md`.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use roster_core::{Grid, PaneId, Session};

mod pane;
mod sidebar;
mod style;

pub use pane::PaneView;
pub use sidebar::{format_age, sidebar_entries, Message, Sidebar, SidebarEntry, SidebarState};
pub use style::{cell_style, state_color, state_glyph, state_label};

/// Columns reserved for the sidebar, when the terminal is wide enough to
/// afford them; narrower terminals give it up to half the width.
pub const SIDEBAR_WIDTH: u16 = 32;

/// Rows reserved at the bottom for the status line.
pub const STATUS_HEIGHT: u16 = 1;

/// Which edge the sidebar occupies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarSide {
    /// Sidebar on the left, panes to its right (the default, herdr-style).
    #[default]
    Left,
    /// Sidebar on the right, panes to its left.
    Right,
}

/// Everything one frame needs: the model, each pane's screen, and the
/// prepared sidebar rows.
pub struct View<'a> {
    /// The session being displayed.
    pub session: &'a Session,
    /// Each pane's current screen grid. Panes without one render blank.
    pub grids: &'a HashMap<PaneId, Grid>,
    /// Panes whose process has exited, with the exit code to show.
    pub exited: &'a HashMap<PaneId, u32>,
    /// Sidebar rows, already built and sorted (see [`sidebar_entries`]).
    pub entries: &'a [SidebarEntry],
    /// The sidebar row to highlight, if any.
    pub selected: Option<usize>,
    /// Which edge the sidebar occupies.
    pub side: SidebarSide,
    /// Mode badge for the status line (e.g. `PREFIX`), when one is active.
    pub mode_badge: Option<&'a str>,
    /// Status line text, shown dim after the badge.
    pub status: &'a str,
}

/// The sidebar width for a frame of `total_width`: the fixed width, or half
/// the frame on terminals too narrow to spare it.
fn sidebar_width(total_width: u16) -> u16 {
    SIDEBAR_WIDTH.min(total_width / 2)
}

/// The absolute region panes are laid out in: everything beside the sidebar
/// and above the status line.
pub fn panes_area(frame_area: Rect, side: SidebarSide) -> Rect {
    let bar = sidebar_width(frame_area.width);
    let x = match side {
        SidebarSide::Left => frame_area.x + bar,
        SidebarSide::Right => frame_area.x,
    };
    Rect::new(
        x,
        frame_area.y,
        frame_area.width - bar,
        frame_area.height.saturating_sub(STATUS_HEIGHT),
    )
}

/// The absolute region the sidebar occupies.
fn sidebar_area(frame_area: Rect, side: SidebarSide) -> Rect {
    let bar = sidebar_width(frame_area.width);
    let x = match side {
        SidebarSide::Left => frame_area.x,
        SidebarSide::Right => frame_area.x + frame_area.width - bar,
    };
    Rect::new(
        x,
        frame_area.y,
        bar,
        frame_area.height.saturating_sub(STATUS_HEIGHT),
    )
}

/// The part of a laid-out pane rect its content actually occupies: one
/// column is given up to a separator on the right edge and one row on the
/// bottom edge, except where the pane touches the pane area's own boundary.
/// `rect` and `area` share an origin; the inset is size-only.
pub fn content_rect(rect: roster_core::Rect, area: Rect) -> Rect {
    let mut width = rect.width;
    let mut height = rect.height;
    if rect.x + rect.width < area.x + area.width {
        width = width.saturating_sub(1);
    }
    if rect.y + rect.height < area.y + area.height {
        height = height.saturating_sub(1);
    }
    Rect::new(rect.x, rect.y, width, height)
}

/// The pane-local layout area: origin `(0, 0)`, sized to the pane region.
/// Pane rects from [`Session::layout`] live in this space.
pub fn local_panes(panes: Rect) -> Rect {
    Rect::new(0, 0, panes.width, panes.height)
}

/// Draw one frame: the active window's panes, the sidebar, and the status
/// line along the bottom. The terminal cursor lands on the focused pane's
/// cursor when that pane's grid shows one.
pub fn render(frame: &mut Frame, view: &View) {
    let area = frame.area();
    let panes = panes_area(area, view.side);
    let local = local_panes(panes);
    let focused = view.session.focused();

    for (id, rect) in view.session.layout(panes.width, panes.height) {
        let content_local = content_rect(rect, local);
        let content = Rect::new(
            panes.x + content_local.x,
            panes.y + content_local.y,
            content_local.width,
            content_local.height,
        );
        let rect_abs =
            roster_core::Rect::new(panes.x + rect.x, panes.y + rect.y, rect.width, rect.height);
        draw_separators(frame.buffer_mut(), rect_abs, content, panes);
        let Some(grid) = view.grids.get(&id) else {
            continue;
        };
        frame.render_widget(PaneView::new(grid), content);

        if let Some(code) = view.exited.get(&id) {
            let notice = format!(" exited ({code}) — ctrl-b x to close ");
            let y = content.y + content.height.saturating_sub(1);
            frame.buffer_mut().set_stringn(
                content.x,
                y,
                &notice,
                usize::from(content.width),
                Style::default().add_modifier(Modifier::REVERSED),
            );
        } else if focused == Some(id) && grid.cursor.visible {
            let (col, row) = (grid.cursor.col as u16, grid.cursor.row as u16);
            if col < content.width && row < content.height {
                frame.set_cursor_position(Position::new(content.x + col, content.y + row));
            }
        }
    }

    frame.render_widget(
        Sidebar::new(view.entries, view.selected, view.session.window_count()),
        sidebar_area(area, view.side),
    );

    draw_status(frame.buffer_mut(), area, view);
}

/// Fill the separator gap to the right of and below a pane with rules.
fn draw_separators(buf: &mut Buffer, rect: roster_core::Rect, content: Rect, panes: Rect) {
    let style = Style::default().add_modifier(Modifier::DIM);
    if content.width < rect.width {
        let x = rect.x + rect.width - 1;
        for y in rect.y..rect.y + rect.height {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char('│');
                cell.set_style(style);
            }
        }
    }
    if content.height < rect.height {
        let y = rect.y + rect.height - 1;
        for x in rect.x..rect.x + content.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char('─');
                cell.set_style(style);
            }
        }
    }
    let _ = panes;
}

fn draw_status(buf: &mut Buffer, area: Rect, view: &View) {
    if area.height < STATUS_HEIGHT {
        return;
    }
    let y = area.y + area.height - STATUS_HEIGHT;
    let mut x = area.x;
    if let Some(badge) = view.mode_badge {
        let text = format!(" {badge} ");
        buf.set_stringn(
            x,
            y,
            &text,
            usize::from(area.width),
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        );
        x += text.chars().count() as u16 + 1;
    }
    if x < area.x + area.width {
        buf.set_stringn(
            x,
            y,
            view.status,
            usize::from(area.x + area.width - x),
            Style::default().add_modifier(Modifier::DIM),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_rect_insets_only_interior_edges() {
        let area = Rect::new(0, 0, 48, 24);
        // Left pane: another pane to its right, none below.
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 24, 24), area),
            Rect::new(0, 0, 23, 24)
        );
        // Right pane touches the area's right edge.
        assert_eq!(
            content_rect(roster_core::Rect::new(24, 0, 24, 12), area),
            Rect::new(24, 0, 24, 11)
        );
        // Bottom-right pane touches both boundaries.
        assert_eq!(
            content_rect(roster_core::Rect::new(24, 12, 24, 12), area),
            Rect::new(24, 12, 24, 12)
        );
    }

    #[test]
    fn panes_area_reserves_sidebar_and_status() {
        let area = Rect::new(0, 0, 120, 30);
        // Left sidebar: panes are offset to its right.
        assert_eq!(
            panes_area(area, SidebarSide::Left),
            Rect::new(32, 0, 88, 29)
        );
        assert_eq!(
            sidebar_area(area, SidebarSide::Left),
            Rect::new(0, 0, 32, 29)
        );
        // Right sidebar: panes start at the left edge.
        assert_eq!(
            panes_area(area, SidebarSide::Right),
            Rect::new(0, 0, 88, 29)
        );
        assert_eq!(
            sidebar_area(area, SidebarSide::Right),
            Rect::new(88, 0, 32, 29)
        );
        // Narrow terminals split the width instead of going negative.
        let narrow = Rect::new(0, 0, 40, 10);
        assert_eq!(
            panes_area(narrow, SidebarSide::Left),
            Rect::new(20, 0, 20, 9)
        );
    }
}
