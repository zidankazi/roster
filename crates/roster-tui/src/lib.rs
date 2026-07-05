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

mod hit;
mod launcher;
mod pane;
mod sidebar;
mod style;

pub use hit::{hit_test, pointer_for, Hit, Pointer};
pub use launcher::{launch_items, LaunchItem, Launcher, LauncherState};
pub use pane::PaneView;
pub use sidebar::{format_age, sidebar_entries, Message, Sidebar, SidebarEntry, SidebarState};
pub use style::{cell_style, state_color, state_glyph, state_label, ACCENT};

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
    /// What the mouse pointer is over, for hover affordances: the ✕ under
    /// it lights up, the + new agent button inverts, sidebar cards get a
    /// hover marker.
    pub hover: Option<Hit>,
    /// Which edge the sidebar occupies.
    pub side: SidebarSide,
    /// The agent launcher, when open: its items and input state.
    pub launcher: Option<(&'a [LaunchItem], &'a LauncherState)>,
    /// Mode badge for the status line (e.g. `PREFIX`), when one is active.
    pub mode_badge: Option<&'a str>,
    /// Status line text, shown dim after the badge.
    pub status: &'a str,
    /// Frame counter; animates the working spinner.
    pub tick: u64,
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

/// The sidebar's content region (the sidebar area minus its rule column),
/// and where clicks on agent cards land.
pub fn sidebar_inner(frame_area: Rect, side: SidebarSide) -> Rect {
    let bar = sidebar_area(frame_area, side);
    match side {
        SidebarSide::Left => Rect::new(bar.x, bar.y, bar.width.saturating_sub(1), bar.height),
        SidebarSide::Right => Rect::new(bar.x + 1, bar.y, bar.width.saturating_sub(1), bar.height),
    }
}

/// The sidebar row hosting the pinned `+ new agent` button, when the
/// sidebar is tall enough to spare it. The button is the mouse-first way
/// into the launcher; `render` draws it and `hit_test` targets it.
pub fn sidebar_button_row(frame_area: Rect, side: SidebarSide) -> Option<u16> {
    let bar = sidebar_inner(frame_area, side);
    (bar.height >= 6).then(|| bar.y + bar.height - 1)
}

/// The columns of a pane's title row occupied by its `✕` close button, in
/// pane-local coordinates: a 3-column target around the glyph at the
/// title's right edge. `None` when the title is too narrow to host one.
pub fn close_button_cols(rect: roster_core::Rect, area: Rect) -> Option<std::ops::Range<u16>> {
    let content = content_rect(rect, area);
    (rect.height >= 2 && content.width >= 12)
        .then(|| rect.x + content.width - 3..rect.x + content.width)
}

/// The part of a laid-out pane rect its content actually occupies: the top
/// row is given to the pane's title bar, and one column to a separator on
/// the right edge when another pane sits beyond it. Stacked panes need no
/// horizontal rule — the lower pane's title bar is the divider. `rect` and
/// `area` share an origin; the inset is size-only.
pub fn content_rect(rect: roster_core::Rect, area: Rect) -> Rect {
    let mut width = rect.width;
    if rect.x + rect.width < area.x + area.width {
        width = width.saturating_sub(1);
    }
    if rect.height < 2 {
        return Rect::new(rect.x, rect.y, width, rect.height);
    }
    Rect::new(rect.x, rect.y + 1, width, rect.height - 1)
}

/// The pane-local layout area: origin `(0, 0)`, sized to the pane region.
/// Pane rects from [`Session::layout`] live in this space.
pub fn local_panes(panes: Rect) -> Rect {
    Rect::new(0, 0, panes.width, panes.height)
}

/// Draw one frame: the active window's panes (each under its title bar),
/// the sidebar, and the status line along the bottom. The terminal cursor
/// lands on the focused pane's cursor — or on the launcher's input when the
/// launcher is open.
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

        // Interior vertical separator to the pane's right.
        if content_local.width < rect.width {
            let x = panes.x + rect.x + rect.width - 1;
            for y in panes.y + rect.y..panes.y + rect.y + rect.height {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_char('│');
                    cell.set_style(Style::default().add_modifier(Modifier::DIM));
                }
            }
        }

        if rect.height >= 2 {
            draw_title(
                frame.buffer_mut(),
                Rect::new(panes.x + rect.x, panes.y + rect.y, content.width, 1),
                view,
                id,
                focused == Some(id),
            );
        }

        let Some(grid) = view.grids.get(&id) else {
            continue;
        };
        frame.render_widget(PaneView::new(grid), content);

        if let Some(code) = view.exited.get(&id) {
            let notice = format!(" exited ({code}) — click ✕ to close ");
            let y = content.y + content.height.saturating_sub(1);
            frame.buffer_mut().set_stringn(
                content.x,
                y,
                &notice,
                usize::from(content.width),
                Style::default().add_modifier(Modifier::REVERSED),
            );
        } else if view.launcher.is_none() && focused == Some(id) && grid.cursor.visible {
            let (col, row) = (grid.cursor.col as u16, grid.cursor.row as u16);
            if col < content.width && row < content.height {
                frame.set_cursor_position(Position::new(content.x + col, content.y + row));
            }
        }
    }

    // The sidebar, with a full-height rule separating it from the panes.
    let bar = sidebar_area(area, view.side);
    let bar_inner = sidebar_inner(area, view.side);
    let rule_x = match view.side {
        SidebarSide::Left => bar.x + bar.width.saturating_sub(1),
        SidebarSide::Right => bar.x,
    };
    for y in bar.y..bar.y + bar.height {
        if let Some(cell) = frame.buffer_mut().cell_mut((rule_x, y)) {
            cell.set_char('│');
            cell.set_style(Style::default().add_modifier(Modifier::DIM));
        }
    }
    let mut cards = bar_inner;
    if let Some(button_y) = sidebar_button_row(area, view.side) {
        // Keep the cards clear of the button and its breathing row.
        cards.height = cards.height.saturating_sub(2);
        let mut button_style = Style::default()
            .fg(style::ACCENT)
            .add_modifier(Modifier::BOLD);
        if view.hover == Some(Hit::SidebarNewAgent) {
            button_style = button_style.add_modifier(Modifier::REVERSED);
        }
        frame.buffer_mut().set_stringn(
            bar_inner.x + 1,
            button_y,
            " + new agent ",
            usize::from(bar_inner.width.saturating_sub(1)),
            button_style,
        );
    }
    let hovered_entry = match view.hover {
        Some(Hit::SidebarEntry(index)) => Some(index),
        _ => None,
    };
    frame.render_widget(
        Sidebar::new(
            view.entries,
            view.selected,
            hovered_entry,
            view.session.window_count(),
            view.tick,
        ),
        cards,
    );

    draw_status(frame.buffer_mut(), area, view);

    if let Some((items, state)) = view.launcher {
        let launcher = Launcher::new(items, state);
        let modal = launcher.modal_rect(area);
        // Cursor follows the launcher's input line.
        let input_len = 2 + state.input().chars().count() as u16;
        frame.set_cursor_position(Position::new(
            (modal.x + 2 + input_len).min(modal.x + modal.width.saturating_sub(2)),
            modal.y + 1,
        ));
        frame.render_widget(launcher, area);
    }
}

/// One pane's title bar: an accent marker on the focused pane, a live state
/// glyph, and the agent or command name. Focus reads as color, not a heavy
/// inverse bar.
fn draw_title(buf: &mut Buffer, span: Rect, view: &View, id: PaneId, focused: bool) {
    if span.width == 0 {
        return;
    }
    let entry = view.entries.iter().find(|e| e.pane == id);
    let (glyph, glyph_style, label) = match entry {
        Some(entry) => (
            state_glyph(entry.state, view.tick),
            Style::default().fg(state_color(entry.state)),
            entry.agent.clone(),
        ),
        None => {
            let command = view
                .session
                .pane(id)
                .and_then(|p| p.command.clone())
                .unwrap_or_default();
            let name = command.split_whitespace().next().unwrap_or("").to_string();
            let name = name.rsplit('/').next().unwrap_or(&name).to_string();
            ("○", Style::default().add_modifier(Modifier::DIM), name)
        }
    };

    if focused {
        buf.set_string(span.x, span.y, "▎", Style::default().fg(style::ACCENT));
    }
    buf.set_stringn(
        span.x + 2,
        span.y,
        glyph,
        usize::from(span.width.saturating_sub(2)),
        glyph_style,
    );
    let text = if view.exited.contains_key(&id) {
        format!("{label} · exited")
    } else {
        label
    };
    let text_style = if focused {
        Style::default()
            .fg(style::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    // Leave room for the ✕ close button at the right edge when it fits.
    let close = span.width >= 12;
    let text_width = if close {
        span.width.saturating_sub(8)
    } else {
        span.width.saturating_sub(5)
    };
    buf.set_stringn(
        span.x + 4,
        span.y,
        text,
        usize::from(text_width),
        text_style,
    );
    if close {
        // The ✕ lights up red under the pointer.
        let style = if view.hover == Some(Hit::PaneClose(id)) {
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        buf.set_string(span.x + span.width - 2, span.y, "✕", style);
    }
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
    fn content_rect_reserves_title_row_and_interior_separator() {
        let area = Rect::new(0, 0, 48, 24);
        // Left pane: another pane to its right → separator column; title row
        // always comes off the top.
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 24, 24), area),
            Rect::new(0, 1, 23, 23)
        );
        // Right pane touches the area's right edge: full width, title row.
        assert_eq!(
            content_rect(roster_core::Rect::new(24, 0, 24, 12), area),
            Rect::new(24, 1, 24, 11)
        );
        // A one-row sliver has no room for a title.
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 48, 1), area),
            Rect::new(0, 0, 48, 1)
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
