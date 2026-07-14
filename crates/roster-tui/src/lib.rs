//! ratatui rendering: pane contents and the agent-state sidebar.
//!
//! Renders a known model into a ratatui buffer — deterministic and
//! snapshot-testable. Input is surfaced as intent [`Message`]s; the binary
//! owns the side effects. See `docs/01-crates.md`.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType};
use ratatui::Frame;
use roster_core::{Grid, PaneId, Session};

mod confirm;
mod exited;
mod hit;
mod launcher;
mod menu;
mod pane;
mod sidebar;
mod style;
mod telemetry;
mod toast;

pub use confirm::{confirm_button_at, confirm_contains, Confirm, ConfirmButton};
pub use exited::{draw_exited, exited_buttons, exited_card_rect};
pub use hit::{hit_test, pointer_for, Hit, HitContext, Pointer};
pub use launcher::{launch_items, LaunchItem, Launcher, LauncherState};
pub use menu::{
    menu_contains, menu_fits, menu_item_at, ContextMenu, ContextMenuItem, ContextMenuView,
};
pub use pane::PaneView;
pub use sidebar::{
    auto_all_cols, auto_chip_cols, format_age, limits_footer_height, pin_to_top, shell_entries,
    shells_height, sidebar_entries, sidebar_rows, Message, ShellEntry, Sidebar, SidebarEntry,
    SidebarRow, SidebarState,
};
pub use style::{
    cell_style, muted, selected, selected_muted, state_color, state_glyph, state_glyph_style,
    ACCENT, SURFACE_BASE,
};
pub use telemetry::{limit_notice_text, telemetry_line};
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
    /// Sidebar on the left, panes to its right (the default).
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
    /// The sidebar's shells-section rows (see [`shell_entries`]): panes
    /// whose command isn't a configured agent. Empty renders no `shells`
    /// section at all.
    pub shells: &'a [ShellEntry],
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
    /// The close-confirmation dialog, when open: the button under the
    /// pointer, if any.
    pub confirm: Option<Option<ConfirmButton>>,
    /// The sidebar card context menu, when open (see [`ContextMenuView`]).
    pub context_menu: Option<ContextMenuView<'a>>,
    /// Live toasts, newest first.
    pub toasts: &'a [(&'a str, ToastLevel)],
    /// The account's fleet-aggregated rate-limit reading, when any pane's
    /// statusline feed is reporting one — the sidebar pins it as a footer.
    /// `None` (no bridge, feed gone stale) renders exactly the sidebar
    /// from before the field existed.
    pub rate_limits: Option<&'a roster_core::RateLimit>,
    /// An in-progress or finished text selection: the pane and its two
    /// endpoints in pane-content coordinates (either order).
    pub selection: Option<Selection>,
    /// Panes scrolled up into history, with how many lines back they sit.
    pub scrolled: &'a HashMap<PaneId, usize>,
    /// Bare start: draw the launcher as the full welcome screen (wordmark
    /// over the picker) instead of the compact modal.
    pub welcome: bool,
    /// Mode badge for the status line (e.g. `PREFIX`), when one is active.
    pub mode_badge: Option<&'a str>,
    /// Status line text, shown dim after the badge.
    pub status: &'a str,
    /// Frame counter; animates the working spinner.
    pub tick: u64,
    /// The workspace folder shown at the top of the sidebar, under the
    /// centered `roster` title — the caller's tilde-collapsed launch
    /// directory. `None` renders the header exactly as it was before
    /// these rows existed (roster-tui reads no filesystem state itself).
    pub workspace: Option<&'a str>,
    /// The wall clock shown beside the workspace, pre-formatted by the
    /// caller — roster-tui does no time-of-day reads of its own.
    pub clock: Option<&'a str>,
}

/// The sidebar width for a frame of `total_width`: the fixed width, or half
/// the frame on terminals too narrow to spare it.
fn sidebar_width(total_width: u16) -> u16 {
    SIDEBAR_WIDTH.min(total_width / 2)
}

/// The chrome inset: how far roster's UI stands back from the terminal
/// edge, in rows — columns double it, since terminal cells are roughly
/// twice as tall as they are wide. [`chrome_area`] is its only consumer;
/// retune the breathing room here.
const INSET: u16 = 1;

/// The area roster's chrome occupies: the frame stood back from the
/// terminal edge by the inset — `2·INSET` columns, `INSET` rows — so the
/// UI reads as an app sitting on the canvas rather than text jammed to the
/// bezel. Small terminals spend every cell on content instead; the guard
/// scales with [`INSET`] so retuning it can't underflow the subtraction.
/// Every public geometry resolver takes the *raw* frame area and applies
/// this exactly once, so render, hit-testing, and the binary can't
/// disagree about where the chrome starts.
pub fn chrome_area(frame_area: Rect) -> Rect {
    if frame_area.width < 4 * INSET + 20 || frame_area.height < 2 * INSET + 6 {
        return frame_area;
    }
    Rect::new(
        frame_area.x + 2 * INSET,
        frame_area.y + INSET,
        frame_area.width - 4 * INSET,
        frame_area.height - 2 * INSET,
    )
}

/// The absolute region panes are laid out in: everything beside the sidebar
/// and above the status line, within the chrome inset.
pub fn panes_area(frame_area: Rect, side: SidebarSide) -> Rect {
    let area = chrome_area(frame_area);
    let bar = sidebar_width(area.width);
    let x = match side {
        SidebarSide::Left => area.x + bar,
        SidebarSide::Right => area.x,
    };
    Rect::new(
        x,
        area.y,
        area.width - bar,
        area.height.saturating_sub(STATUS_HEIGHT),
    )
}

/// The absolute region the sidebar occupies, within the chrome inset.
fn sidebar_area(frame_area: Rect, side: SidebarSide) -> Rect {
    let area = chrome_area(frame_area);
    let bar = sidebar_width(area.width);
    let x = match side {
        SidebarSide::Left => area.x,
        SidebarSide::Right => area.x + area.width - bar,
    };
    Rect::new(x, area.y, bar, area.height.saturating_sub(STATUS_HEIGHT))
}

/// The sidebar's content region — the sidebar area minus the gap column
/// that stands it off from the pane panels (regions separate by spacing
/// and background, not rules) — and where clicks on agent cards land.
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

/// Whether a pane rect gets its rounded panel. Below 3×3 there is no room
/// for a border around any content, so the sliver spends every cell on the
/// guest screen instead.
pub(crate) fn panelled(rect: roster_core::Rect) -> bool {
    rect.width >= 3 && rect.height >= 3
}

/// The columns of a pane's top border occupied by its `✕` close button, in
/// pane-local coordinates: a 3-column target around the glyph, one column
/// in from the panel's corner. `None` when the border is too narrow to
/// host one.
pub fn close_button_cols(rect: roster_core::Rect) -> Option<std::ops::Range<u16>> {
    (panelled(rect) && rect.width >= 14).then(|| rect.x + rect.width - 4..rect.x + rect.width - 1)
}

/// The status row's right-aligned workspace indicator — `⧉ 2/3`, clickable
/// to cycle windows. `None` with a single window (nothing to indicate) or
/// when the status row is too crowded to fit it.
pub fn status_windows_span(
    frame_area: Rect,
    active: usize,
    count: usize,
) -> Option<(Rect, String)> {
    let area = chrome_area(frame_area);
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

/// The `grid` layout pill's text — space-padded into the reverse-video
/// pill `style::chip` draws. `status_view_spans` derives the click-target
/// width from these consts, so a relabel can't strand the targets.
const GRID_PILL: &str = " grid ";

/// The `solo` layout pill's text; see [`GRID_PILL`].
const SOLO_PILL: &str = " solo ";

/// The status row's `grid` / `solo` layout-switcher pills, sitting left of
/// the workspace indicator (`windows` is its rect, when shown). `None` with
/// a single pane — the layouts are identical and the control would be
/// noise — or when the row is too crowded to keep the pills clear of the
/// key hints.
fn status_view_spans(
    frame_area: Rect,
    windows: Option<&Rect>,
    multi_pane: bool,
) -> Option<(Rect, Rect)> {
    let area = chrome_area(frame_area);
    if !multi_pane || area.height < STATUS_HEIGHT {
        return None;
    }
    let right = windows.map(|rect| rect.x).unwrap_or(area.x + area.width);
    let y = area.y + area.height - STATUS_HEIGHT;
    // The two pills, one gap column apart, one column in from whatever
    // bounds them on the right.
    let grid = GRID_PILL.chars().count() as u16;
    let solo = SOLO_PILL.chars().count() as u16;
    let total = grid + 1 + solo + 1;
    ((right - area.x) > total + 24).then(|| {
        let solo_x = right - 1 - solo;
        let grid_x = solo_x - 1 - grid;
        (Rect::new(grid_x, y, grid, 1), Rect::new(solo_x, y, solo, 1))
    })
}

/// The status row's clickable controls, resolved in one place: render
/// draws exactly this and `hit_test` targets exactly this, so the drawn
/// chrome and the click targets can't disagree about geometry.
pub struct StatusControls {
    /// The `⧉ N/M` workspace indicator's rect and text, when shown.
    pub windows: Option<(Rect, String)>,
    /// The `(grid, solo)` layout-pill rects, when shown.
    pub views: Option<(Rect, Rect)>,
}

/// Resolve the status row's clickable controls for a frame (see
/// [`StatusControls`]).
pub fn status_controls(area: Rect, side: SidebarSide, session: &Session) -> StatusControls {
    let windows = status_windows_span(
        area,
        session.active_window().unwrap_or(0),
        session.window_count(),
    );
    let panes = panes_area(area, side);
    let multi_pane = session.layout(panes.width, panes.height).len() > 1;
    let views = status_view_spans(area, windows.as_ref().map(|(rect, _)| rect), multi_pane);
    StatusControls { windows, views }
}

/// The part of a laid-out pane rect its content actually occupies: the
/// interior of the pane's rounded panel — one border row and column on
/// every side, the title riding the top border. Rects too small for a
/// panel (see [`panelled`]) keep every cell and draw no border.
pub fn content_rect(rect: roster_core::Rect) -> Rect {
    if !panelled(rect) {
        return Rect::new(rect.x, rect.y, rect.width, rect.height);
    }
    Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2)
}

/// Draw one frame: the active window's panes (each under its title bar),
/// the sidebar, and the status line along the bottom. The terminal cursor
/// lands on the focused pane's cursor — or on the launcher's input when the
/// launcher is open.
pub fn render(frame: &mut Frame, view: &View) {
    let area = frame.area();

    // The whole frame sits on the base canvas: the inset margin, the gap
    // between regions, and under every panel. Regions separate by spacing
    // and surface, not rules. Painted before the welcome branch too, so
    // the first agent launch doesn't flash the background.
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(style::SURFACE_BASE));

    // The bare-start opening screen owns the whole frame: the animated
    // wordmark over the picker, dead-centered — no sidebar, no status.
    if view.welcome {
        if let Some((items, state)) = view.launcher {
            let launcher = Launcher::new(items, state).welcome(true).tick(view.tick);
            if let Some((cursor_x, cursor_y)) = launcher.input_position(area) {
                frame.set_cursor_position(Position::new(cursor_x, cursor_y));
            }
            frame.render_widget(launcher, area);
            // Launch failures must reach the user even on the welcome
            // screen.
            draw_toasts(frame.buffer_mut(), area, view.toasts);
            return;
        }
    }

    let panes = panes_area(area, view.side);
    let focused = view.session.focused();

    // Solo view shows only the focused pane, full size; the grid otherwise.
    let rects: Vec<(PaneId, roster_core::Rect)> = match (view.zoomed, focused) {
        (true, Some(id)) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
        _ => view.session.layout(panes.width, panes.height),
    };
    for (id, rect) in rects {
        let content_local = content_rect(rect);
        let content = Rect::new(
            panes.x + content_local.x,
            panes.y + content_local.y,
            content_local.width,
            content_local.height,
        );

        // The pane's rounded panel; focus reads as the accent border, one
        // vocabulary with the sidebar's inverted card. The title rides the
        // top border.
        if panelled(rect) {
            let border_style = if focused == Some(id) {
                Style::default().fg(style::ACCENT)
            } else {
                style::muted()
            };
            let panel = Rect::new(panes.x + rect.x, panes.y + rect.y, rect.width, rect.height);
            frame.render_widget(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border_style),
                panel,
            );
            // The ✕ range comes from the one resolver hit_test targets,
            // translated to absolute columns, so the drawn glyph can't
            // drift off its click target.
            let close =
                close_button_cols(rect).map(|cols| panes.x + cols.start..panes.x + cols.end);
            draw_title(
                frame.buffer_mut(),
                Rect::new(panel.x, panel.y, panel.width, 1),
                view,
                id,
                focused == Some(id),
                close,
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
                    style::accent_pill(),
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
                // Only promise a ✕ where the border actually hosts one —
                // a sliver pane has no close button to click.
                let notice = if close_button_cols(rect).is_some() {
                    format!(" exited ({code}) — click ✕ to close ")
                } else {
                    format!(" exited ({code}) ")
                };
                let y = content.y + content.height.saturating_sub(1);
                // The selected surface, not a bare reversal: over frozen
                // guest cells a REVERSED strip would swap against whatever
                // each cell held — this pins one legible fill.
                frame.buffer_mut().set_stringn(
                    content.x,
                    y,
                    &notice,
                    usize::from(content.width),
                    style::selected(),
                );
            }
        } else if view.launcher.is_none()
            && view.confirm.is_none()
            && view.context_menu.is_none()
            && focused == Some(id)
            && grid.cursor.visible
        {
            let (col, row) = (grid.cursor.col as u16, grid.cursor.row as u16);
            if col < content.width && row < content.height {
                frame.set_cursor_position(Position::new(content.x + col, content.y + row));
            }
        }
    }

    // The sidebar: no rule against the panes — the gap column and the
    // surface change are the separation. (The frame-wide base fill above
    // already covers the gap and the pinned button row; the widget fills
    // its own card area too so it stays self-contained in tests.)
    let bar_inner = sidebar_inner(area, view.side);
    let mut cards = bar_inner;
    if let Some(button_y) = sidebar_button_row(area, view.side) {
        // Keep the cards clear of the button and its breathing row.
        cards.height = cards.height.saturating_sub(2);
        frame.buffer_mut().set_stringn(
            bar_inner.x + 1,
            button_y,
            " + new agent ",
            usize::from(bar_inner.width.saturating_sub(1)),
            style::chip(false, view.hover == Some(Hit::SidebarNewAgent), false),
        );
    }
    let hovered_entry = match view.hover {
        Some(Hit::SidebarEntry(index)) => Some(index),
        _ => None,
    };
    let hovered_auto = match view.hover {
        Some(Hit::SidebarAuto(index)) => Some(index),
        _ => None,
    };
    // The card whose pane holds focus carries the accent bar. Entries span
    // every workspace but focus is the active window's, so at most one card
    // matches.
    let focused_entry = sidebar::focused_entry(view.entries, focused);
    frame.render_widget(
        Sidebar::new(
            view.entries,
            view.selected,
            hovered_entry,
            view.session.window_count(),
            view.tick,
        )
        .shells(view.shells)
        .focused(focused_entry)
        .hovered_auto(hovered_auto)
        .hovered_auto_all(view.hover == Some(Hit::SidebarAutoAll))
        .rate_limits(view.rate_limits)
        .workspace(view.workspace)
        .clock(view.clock)
        .hovered_workspace(view.hover == Some(Hit::SidebarWorkspace)),
        cards,
    );

    draw_status(frame.buffer_mut(), area, view);
    draw_toasts(frame.buffer_mut(), area, view.toasts);

    if let Some((items, state)) = view.launcher {
        let launcher = Launcher::new(items, state);
        // Cursor follows the launcher's input line — and hides with it.
        if let Some((cursor_x, cursor_y)) = launcher.input_position(area) {
            frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
        frame.render_widget(launcher, area);
    }

    if let Some(hover) = view.confirm {
        frame.render_widget(Confirm::new().hover(hover), area);
    }

    // The context menu sits above everything, including the other modals —
    // nothing else opens while it owns the mouse, so ordering only guards a
    // stale frame mid-transition.
    if let Some(menu) = &view.context_menu {
        frame.render_widget(
            ContextMenu::new(menu.items, menu.anchor).hover(menu.hover),
            area,
        );
    }
}

/// A pane's display name: its agent card's [`SidebarEntry::display_name`]
/// when detected — the live task title, falling back to the config name —
/// else [`shell_entries`]'s own resolver ([`shell_display_name`]). One
/// resolver for the pane border, the exited card, and (via the entry
/// method or `shell_display_name`) the sidebar card, so the surfaces can't
/// disagree about what a pane is called.
fn pane_display_name(view: &View, id: PaneId) -> String {
    if let Some(entry) = view.entries.iter().find(|e| e.pane == id) {
        return entry.display_name().to_string();
    }
    let pane = view.session.pane(id);
    let command = pane.and_then(|p| p.command.as_deref()).unwrap_or_default();
    sidebar::shell_display_name(pane.and_then(|p| p.title.as_deref()), command)
}

/// One pane's title, riding the panel's top border: a space-padded live
/// state glyph and the pane's display name breaking the border line —
/// the live session title when the agent set one (the border usually has
/// the width the sidebar card lacks, so the task shows in full where the
/// card truncates), else
/// `╭ ⠋ claude-code ─╮`. Focus reads as the accent border and an accented
/// name, not a heavy inverse bar. `span` is the panel's full top row;
/// `close` is the ✕ button's absolute columns from `close_button_cols`,
/// when the border hosts one — the caller resolves it so the drawn glyph
/// and the hit target share one source.
/// The border marker an exited pane's title carries. `draw_title` keeps it
/// visible by truncating the task label instead of the marker.
const EXITED_SUFFIX: &str = " · exited";

fn draw_title(
    buf: &mut Buffer,
    span: Rect,
    view: &View,
    id: PaneId,
    focused: bool,
    close: Option<std::ops::Range<u16>>,
) {
    // Corners, pads, and the glyph need this much before any name fits.
    if span.width < 7 {
        return;
    }
    // The shared glyph style keeps the title in step with the sidebar
    // card: the same done pane pulses in both places. The label shares
    // `pane_display_name` with the exited card, so a pane is called the
    // same thing everywhere.
    let (glyph, glyph_style) = match view.entries.iter().find(|e| e.pane == id) {
        Some(entry) => (
            state_glyph(entry.state, view.tick),
            style::state_glyph_style(entry.state, view.tick),
        ),
        None => ("○", style::muted()),
    };
    let label = pane_display_name(view, id);

    // ` ⠋ name ` punched into the border line, pads included, so the text
    // never touches the box-drawing chars.
    buf.set_string(span.x + 1, span.y, " ", style::muted());
    buf.set_string(span.x + 2, span.y, glyph, glyph_style);
    buf.set_string(span.x + 3, span.y, " ", style::muted());
    let text_style = if focused {
        Style::default()
            .fg(style::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        style::muted()
    };
    // The name stops one pad short of the ✕ target (or of the corner when
    // there is none).
    let text_end = match &close {
        Some(cols) => cols.start.saturating_sub(span.x),
        None => span.width - 2,
    };
    let budget = usize::from(text_end.saturating_sub(4));
    let text = if view.exited.contains_key(&id) {
        // The exit marker survives any task title: the label yields the
        // cells, since a truncated name still reads but a truncated marker
        // vanishes. The label's cut is cell-budgeted (the suffix itself is
        // ASCII) so a run of double-width glyphs cannot cost the suffix
        // its tail.
        let room = budget.saturating_sub(EXITED_SUFFIX.chars().count());
        format!("{}{EXITED_SUFFIX}", sidebar::truncate(&label, room))
    } else {
        label
    };
    if budget > 0 {
        // set_stringn reports where the paint actually ended — cells, not
        // chars — so the border-repair pad can't land inside a
        // double-width glyph.
        let (end_x, _) = buf.set_stringn(span.x + 4, span.y, text, budget, text_style);
        if end_x < span.x + span.width - 1 {
            buf.set_string(end_x, span.y, " ", style::muted());
        }
    }
    if let Some(cols) = close {
        // ` ✕ ` filling the resolver's 3-column target; it lights up red
        // under the pointer.
        let style = if view.hover == Some(Hit::PaneClose(id)) {
            Style::default()
                .fg(style::danger())
                .add_modifier(Modifier::BOLD)
        } else {
            style::muted()
        };
        buf.set_string(cols.start, span.y, " ", style::muted());
        buf.set_string(cols.start + 1, span.y, "✕", style);
        buf.set_string(cols.start + 2, span.y, " ", style::muted());
    }
}

fn draw_status(buf: &mut Buffer, area: Rect, view: &View) {
    let inner = chrome_area(area);
    if inner.height < STATUS_HEIGHT {
        return;
    }
    let y = inner.y + inner.height - STATUS_HEIGHT;
    let mut x = inner.x;
    if let Some(badge) = view.mode_badge {
        let text = format!(" {badge} ");
        // A pinned bright pill: reversing an unset foreground would fill
        // the badge with the terminal's themed default instead.
        buf.set_stringn(
            x,
            y,
            &text,
            usize::from(inner.width),
            style::bright().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        );
        x += text.chars().count() as u16 + 1;
    }
    // The workspace indicator claims the right edge, the layout-switcher
    // pills sit left of it; the hint text yields to both. `area` is the
    // raw frame — status_controls applies the chrome inset itself.
    let StatusControls {
        windows: span,
        views,
    } = status_controls(area, view.side, view.session);
    let right_edge = views
        .as_ref()
        .map(|(grid, _)| grid.x)
        .or_else(|| span.as_ref().map(|(rect, _)| rect.x))
        .unwrap_or(inner.x + inner.width);
    // The hint bar centers in the footer — a padded bar, not text jammed
    // into a corner. Centering never costs content: the bar shifts left
    // as far as the badge to clear the right controls before it accepts
    // truncation.
    let width = hotkeys_width(view.status);
    let centered = inner.x + inner.width.saturating_sub(width) / 2;
    let x = centered.min(right_edge.saturating_sub(width)).max(x);
    if x < right_edge {
        draw_hotkeys(buf, x, y, right_edge - x, view.status);
    }
    if let Some((grid, solo)) = views {
        // The active layout's pill is the armed one — the switcher doubles
        // as the mode indicator.
        buf.set_string(
            grid.x,
            grid.y,
            GRID_PILL,
            style::chip(!view.zoomed, view.hover == Some(Hit::StatusViewGrid), false),
        );
        buf.set_string(
            solo.x,
            solo.y,
            SOLO_PILL,
            style::chip(view.zoomed, view.hover == Some(Hit::StatusViewSolo), false),
        );
    }
    if let Some((rect, text)) = span {
        let style = if view.hover == Some(Hit::StatusWindows) {
            Style::default()
                .fg(style::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
                .fg(style::ACCENT)
                .add_modifier(Modifier::BOLD)
        };
        buf.set_string(rect.x + 1, rect.y, &text, style);
    }
}

/// The separator drawn between hint segments — wide gaps so the centered
/// footer reads as spaced controls, not a sentence, sized so the longest
/// mode palette (PREFIX) still fits a 120-column terminal. `hotkeys_width`
/// derives the centering math from this constant, so the two can't drift.
const HINT_GAP: &str = "  ·  ";

/// Draw a status hint as a hotkey bar: within each ` · `-separated segment,
/// a `key: label` pair renders the key accented and the label muted, so the
/// keys read at a glance; a segment without the colon is plain muted text.
/// This is how every mode's hint string gets its keys highlighted without
/// the modes agreeing on anything beyond the `key: label` grammar. Segments
/// are set off by the wide [`HINT_GAP`].
fn draw_hotkeys(buf: &mut Buffer, x: u16, y: u16, budget: u16, status: &str) {
    let mut remaining = usize::from(budget);
    let mut x = x;
    let mut put = |x: &mut u16, text: &str, style: Style| {
        if remaining == 0 {
            return;
        }
        buf.set_stringn(*x, y, text, remaining, style);
        let used = text.chars().count().min(remaining);
        remaining -= used;
        *x += used as u16;
    };
    let key_style = Style::default()
        .fg(style::ACCENT)
        .add_modifier(Modifier::BOLD);
    for (i, segment) in status.split(" · ").enumerate() {
        if i > 0 {
            put(&mut x, HINT_GAP, style::muted());
        }
        match segment.split_once(": ") {
            Some((key, label)) => {
                put(&mut x, key, key_style);
                put(&mut x, " ", style::muted());
                put(&mut x, label, style::muted());
            }
            None => put(&mut x, segment, style::muted()),
        }
    }
}

/// The exact cells [`draw_hotkeys`] paints for `status`, for centering:
/// segment widths, the wide gaps between them, and the one-cell saving of
/// each `key: label` colon rendering as a space.
fn hotkeys_width(status: &str) -> u16 {
    let mut width = 0usize;
    for (i, segment) in status.split(" · ").enumerate() {
        if i > 0 {
            width += HINT_GAP.chars().count();
        }
        width += segment.chars().count();
        if segment.split_once(": ").is_some() {
            width -= 1;
        }
    }
    width.min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_bar_accents_keys_and_mutes_labels_and_prose() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 1));
        draw_hotkeys(
            &mut buf,
            0,
            0,
            60,
            "plain prose · ctrl-b: keys · then c: new agent",
        );
        let row: String = (0..60)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string();
        // The colon in `key: label` renders as a space, and segments sit a
        // wide gap apart — spaced controls, not a sentence.
        assert_eq!(row, "plain prose  ·  ctrl-b keys  ·  then c new agent");

        let accent = Style::default()
            .fg(style::ACCENT)
            .add_modifier(Modifier::BOLD);
        let style_at = |x: u16| buf.cell((x, 0)).unwrap().style();
        // Prose segment: muted, never accented.
        assert_eq!(style_at(0).fg, style::muted().fg);
        // "ctrl-b" (cols 16..22) is the key — accent + bold.
        assert_eq!(style_at(16).fg, accent.fg);
        assert!(style_at(16).add_modifier.contains(Modifier::BOLD));
        // Its label "keys" (from col 23) is muted.
        assert_eq!(style_at(23).fg, style::muted().fg);
        // "then c" is a compound key — accented too.
        assert_eq!(style_at(32).fg, accent.fg);
    }

    #[test]
    fn hotkey_bar_respects_its_budget() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        // A budget shorter than the text must clip, never wrap or panic.
        draw_hotkeys(&mut buf, 0, 0, 10, "c: new agent · q: quit");
        let row: String = (0..20)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string();
        assert_eq!(row.chars().count(), 10, "clipped to budget: {row:?}");
    }

    #[test]
    fn content_rect_is_the_panel_interior_and_slivers_keep_every_cell() {
        // A panelled pane loses one border row/column on every side.
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 24, 24)),
            Rect::new(1, 1, 22, 22)
        );
        assert_eq!(
            content_rect(roster_core::Rect::new(24, 0, 24, 12)),
            Rect::new(25, 1, 22, 10)
        );
        // Slivers under 3×3 draw no panel and keep every cell.
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 48, 2)),
            Rect::new(0, 0, 48, 2)
        );
        assert_eq!(
            content_rect(roster_core::Rect::new(0, 0, 2, 24)),
            Rect::new(0, 0, 2, 24)
        );
    }

    #[test]
    fn chrome_stands_back_from_the_edge_when_there_is_room() {
        // A roomy frame is inset by 2 columns and 1 row on every side.
        assert_eq!(
            chrome_area(Rect::new(0, 0, 120, 30)),
            Rect::new(2, 1, 116, 28)
        );
        // Small terminals spend every cell on content.
        assert_eq!(
            chrome_area(Rect::new(0, 0, 23, 30)),
            Rect::new(0, 0, 23, 30)
        );
        assert_eq!(
            chrome_area(Rect::new(0, 0, 120, 7)),
            Rect::new(0, 0, 120, 7)
        );
        assert_eq!(chrome_area(Rect::new(0, 0, 0, 0)), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn panes_area_reserves_sidebar_and_status_inside_the_inset() {
        let area = Rect::new(0, 0, 120, 30);
        // Chrome inset (2,1); left sidebar: panes offset to its right.
        assert_eq!(
            panes_area(area, SidebarSide::Left),
            Rect::new(34, 1, 84, 27)
        );
        assert_eq!(
            sidebar_area(area, SidebarSide::Left),
            Rect::new(2, 1, 32, 27)
        );
        // Right sidebar: panes start at the chrome's left edge.
        assert_eq!(
            panes_area(area, SidebarSide::Right),
            Rect::new(2, 1, 84, 27)
        );
        assert_eq!(
            sidebar_area(area, SidebarSide::Right),
            Rect::new(86, 1, 32, 27)
        );
        // Narrow terminals split the width instead of going negative.
        let narrow = Rect::new(0, 0, 40, 10);
        assert_eq!(
            panes_area(narrow, SidebarSide::Left),
            Rect::new(20, 1, 18, 7)
        );
    }

    #[test]
    fn hint_bar_width_matches_what_draw_hotkeys_paints() {
        for status in [
            "plain prose",
            "c: new agent",
            "c: new agent · q: quit",
            "plain · ctrl-b: keys · x: close",
            "",
        ] {
            let width = hotkeys_width(status);
            let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
            draw_hotkeys(&mut buf, 0, 0, 120, status);
            let drawn = (0..120)
                .rev()
                .find(|x| buf.cell((*x, 0)).unwrap().symbol() != " ")
                .map(|x| x + 1)
                .unwrap_or(0);
            assert_eq!(width, drawn, "status {status:?}");
        }
    }
}
