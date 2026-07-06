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

mod confirm;
mod exited;
mod hit;
mod launcher;
mod pane;
mod rename;
mod sidebar;
mod style;
mod toast;

pub use confirm::{confirm_button_at, confirm_contains, Confirm, ConfirmButton};
pub use exited::{draw_exited, exited_buttons, exited_card_rect};
pub use rename::{draw_rename, rename_contains, rename_cursor, rename_rect};
pub use hit::{hit_test, pointer_for, Hit, Pointer};
pub use launcher::{launch_items, LaunchItem, Launcher, LauncherState};
pub use pane::PaneView;
pub use sidebar::{
    format_age, sidebar_entries, sidebar_rows, Message, Sidebar, SidebarEntry, SidebarRow,
    SidebarState,
};
pub use style::{cell_style, state_color, state_glyph, state_label, ACCENT};
pub use toast::{draw_toasts, toast_rects, ToastLevel};

/// A text selection: the pane and its two `(col, row)` endpoints in
/// pane-content coordinates, in either order.
pub type Selection = (PaneId, (u16, u16), (u16, u16));

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
    /// Solo view: the focused pane fills the whole pane region and the
    /// sidebar becomes the switcher. `false` shows the tiled grid.
    pub zoomed: bool,
    /// Which edge the sidebar occupies.
    pub side: SidebarSide,
    /// The agent launcher, when open: its items and input state.
    pub launcher: Option<(&'a [LaunchItem], &'a LauncherState)>,
    /// The close-confirmation dialog, when open: the threatened agent's
    /// name and the button under the pointer.
    pub confirm: Option<(&'a str, Option<ConfirmButton>)>,
    /// Live toasts, newest first.
    pub toasts: &'a [(&'a str, ToastLevel)],
    /// An in-progress or finished text selection: the pane and its two
    /// endpoints in pane-content coordinates (either order).
    pub selection: Option<Selection>,
    /// Panes scrolled up into history, with how many lines back they sit.
    pub scrolled: &'a HashMap<PaneId, usize>,
    /// Resolved workspace display names, one per window: a manual name or
    /// a live terminal title; empty entries fall back to `workspace N`.
    pub window_names: &'a [String],
    /// The workspace-rename dialog, when open: the window index and the
    /// input typed so far.
    pub rename: Option<(usize, &'a str)>,
    /// Bare start: draw the launcher as the full welcome screen (wordmark
    /// over the picker) instead of the compact modal.
    pub welcome: bool,
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

/// The sidebar row hosting the `grid · solo` layout switcher, one row above
/// the + new agent button. Rendered (and clickable) only when the active
/// window has more than one pane — with a single pane the layouts are
/// identical and the control would be noise.
pub fn sidebar_view_row(frame_area: Rect, side: SidebarSide) -> Option<u16> {
    let bar = sidebar_inner(frame_area, side);
    (bar.height >= 8).then(|| bar.y + bar.height - 2)
}

/// The switcher's click targets on its row, in sidebar-inner columns:
/// `(grid, solo)` word spans.
pub fn view_toggle_cols() -> (std::ops::Range<u16>, std::ops::Range<u16>) {
    // " grid · solo" — generous targets around each word.
    (0..6, 7..13)
}

/// The status row's right-aligned workspace indicator — `⧉ 2/3`, clickable
/// to cycle windows. `None` with a single window (nothing to indicate) or
/// when the status row is too crowded to fit it.
pub fn status_windows_span(area: Rect, active: usize, count: usize) -> Option<(Rect, String)> {
    if count <= 1 || area.height < STATUS_HEIGHT {
        return None;
    }
    let text = format!("⧉ {}/{}", active + 1, count);
    let width = text.chars().count() as u16 + 2;
    (area.width > width + 24).then(|| {
        (
            Rect::new(
                area.x + area.width - width,
                area.y + area.height - STATUS_HEIGHT,
                width,
                1,
            ),
            text,
        )
    })
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

    // The bare-start opening screen owns the whole frame: the animated
    // wordmark over the picker, dead-centered — no sidebar, no status.
    if view.welcome {
        if let Some((items, state)) = view.launcher {
            let launcher = Launcher::new(items, state).welcome(true).tick(view.tick);
            let (cursor_x, cursor_y) = launcher.input_position(area);
            frame.set_cursor_position(Position::new(cursor_x, cursor_y));
            frame.render_widget(launcher, area);
            // Launch failures must reach the user even on the welcome
            // screen.
            draw_toasts(frame.buffer_mut(), area, view.toasts);
            return;
        }
    }

    let panes = panes_area(area, view.side);
    let local = local_panes(panes);
    let focused = view.session.focused();

    // Solo view shows only the focused pane, full size; the grid otherwise.
    let rects: Vec<(PaneId, roster_core::Rect)> = match (view.zoomed, focused) {
        (true, Some(id)) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
        _ => view.session.layout(panes.width, panes.height),
    };
    for (id, rect) in rects {
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
        let selection = match view.selection {
            Some((pane, a, b)) if pane == id => Some((a, b)),
            _ => None,
        };
        frame.render_widget(PaneView::new(grid).selection(selection), content);

        // A chip in the pane's top-right corner while scrolled into
        // history: how far back, and that the view isn't live.
        if let Some(offset) = view.scrolled.get(&id).filter(|o| **o > 0) {
            let chip = format!(" ↑ {offset} ");
            let chip_len = chip.chars().count() as u16;
            if content.width > chip_len + 2 {
                frame.buffer_mut().set_string(
                    content.x + content.width - chip_len - 1,
                    content.y,
                    &chip,
                    Style::default()
                        .fg(style::ACCENT)
                        .add_modifier(Modifier::REVERSED | Modifier::BOLD),
                );
            }
        }

        if let Some(code) = view.exited.get(&id) {
            // A real card with restart/close buttons; panes too small for
            // it keep the one-line strip.
            let name = pane_display_name(view, id);
            let drawn = draw_exited(
                frame.buffer_mut(),
                content,
                &name,
                *code,
                view.hover == Some(Hit::PaneRestart(id)),
                view.hover == Some(Hit::PaneClose(id)),
            );
            if !drawn {
                let notice = format!(" exited ({code}) — click ✕ to close ");
                let y = content.y + content.height.saturating_sub(1);
                frame.buffer_mut().set_stringn(
                    content.x,
                    y,
                    &notice,
                    usize::from(content.width),
                    Style::default().add_modifier(Modifier::REVERSED),
                );
            }
        } else if view.launcher.is_none()
            && view.confirm.is_none()
            && view.rename.is_none()
            && focused == Some(id)
            && grid.cursor.visible
        {
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
    // The grid · solo switcher, above the button, when there is anything
    // to switch between.
    let multi_pane = view.session.layout(panes.width, panes.height).len() > 1;
    if multi_pane {
        if let Some(view_y) = sidebar_view_row(area, view.side) {
            cards.height = cards.height.saturating_sub(1);
            let word = |active: bool, hovered: bool| -> Style {
                let mut style = if active {
                    Style::default()
                        .fg(style::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                };
                if hovered {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                style
            };
            let buf = frame.buffer_mut();
            buf.set_string(
                bar_inner.x + 1,
                view_y,
                "grid",
                word(!view.zoomed, view.hover == Some(Hit::SidebarViewGrid)),
            );
            buf.set_string(
                bar_inner.x + 6,
                view_y,
                "·",
                Style::default().add_modifier(Modifier::DIM),
            );
            buf.set_string(
                bar_inner.x + 8,
                view_y,
                "solo",
                word(view.zoomed, view.hover == Some(Hit::SidebarViewSolo)),
            );
        }
    }
    let hovered_entry = match view.hover {
        Some(Hit::SidebarEntry(index)) => Some(index),
        _ => None,
    };
    let hovered_window = match view.hover {
        Some(Hit::SidebarWindow(window)) => Some(window),
        _ => None,
    };
    frame.render_widget(
        Sidebar::new(
            view.entries,
            view.selected,
            hovered_entry,
            view.session.window_count(),
            view.tick,
        )
        .active(view.session.active_window().unwrap_or(0))
        .hovered_window(hovered_window)
        .names(view.window_names),
        cards,
    );

    draw_status(frame.buffer_mut(), area, view);
    draw_toasts(frame.buffer_mut(), area, view.toasts);

    if let Some((items, state)) = view.launcher {
        let launcher = Launcher::new(items, state);
        // Cursor follows the launcher's input line.
        let (cursor_x, cursor_y) = launcher.input_position(area);
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        frame.render_widget(launcher, area);
    }

    if let Some((name, hover)) = view.confirm {
        frame.render_widget(Confirm::new(name).hover(hover), area);
    }

    if let Some((window, input)) = view.rename {
        draw_rename(frame.buffer_mut(), area, window, input);
        let (cursor_x, cursor_y) = rename_cursor(area, input);
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

/// A pane's display name: its agent card's name when detected, else the
/// basename of its command's binary.
fn pane_display_name(view: &View, id: PaneId) -> String {
    if let Some(entry) = view.entries.iter().find(|e| e.pane == id) {
        return entry.agent.clone();
    }
    let command = view
        .session
        .pane(id)
        .and_then(|p| p.command.clone())
        .unwrap_or_default();
    let name = command.split_whitespace().next().unwrap_or("").to_string();
    name.rsplit('/').next().unwrap_or(&name).to_string()
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
    // The workspace indicator claims the right edge; the hint text yields
    // to it.
    let span = status_windows_span(
        area,
        view.session.active_window().unwrap_or(0),
        view.session.window_count(),
    );
    let right_edge = span
        .as_ref()
        .map(|(rect, _)| rect.x)
        .unwrap_or(area.x + area.width);
    if x < right_edge {
        buf.set_stringn(
            x,
            y,
            view.status,
            usize::from(right_edge - x),
            Style::default().add_modifier(Modifier::DIM),
        );
    }
    if let Some((rect, text)) = span {
        let style = if view.hover == Some(Hit::StatusWindows) {
            Style::default()
                .fg(style::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().fg(style::ACCENT).add_modifier(Modifier::BOLD)
        };
        buf.set_string(rect.x + 1, rect.y, &text, style);
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
