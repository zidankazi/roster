//! Mouse hit-testing: map a click position to what it landed on.
//!
//! Mirrors the geometry `render` draws — the sidebar cards, pane titles and
//! content, and the status line — so the binary can translate mouse events
//! into the same intents keys produce.

use ratatui::layout::Rect;
use roster_core::{PaneId, Session};

use crate::sidebar::SidebarEntry;
use crate::{
    close_button_cols, local_panes, panes_area, sidebar_button_row, sidebar_inner,
    sidebar_view_row, view_toggle_cols, SidebarSide, STATUS_HEIGHT,
};

/// What a screen position corresponds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A sidebar agent card.
    SidebarEntry(usize),
    /// The sidebar's pinned `+ new agent` button.
    SidebarNewAgent,
    /// The `grid` half of the sidebar's layout switcher.
    SidebarViewGrid,
    /// The `solo` half of the sidebar's layout switcher.
    SidebarViewSolo,
    /// Sidebar background (header, spacers, rule).
    Sidebar,
    /// A pane's title bar.
    PaneTitle(PaneId),
    /// A pane title's `✕` close button.
    PaneClose(PaneId),
    /// A pane's content area (or its separator column).
    Pane(PaneId),
    /// The status line.
    Status,
    /// Nothing interactive.
    Outside,
}

/// The mouse pointer shape the terminal should show, keyed to what the
/// pointer is over. Emitted by the binary as an OSC 22 sequence (xterm
/// pointer-shape protocol; Ghostty, Kitty, WezTerm, iTerm2 honor it,
/// others ignore it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pointer {
    /// The terminal's default arrow.
    #[default]
    Default,
    /// A hand — over anything clickable.
    Hand,
    /// An I-beam — over live terminal content.
    Text,
    /// Horizontal resize arrows — over a vertical divider.
    ResizeEw,
    /// Vertical resize arrows — over a horizontal divider.
    ResizeNs,
}

impl Pointer {
    /// The xcursor shape name for OSC 22.
    pub fn name(self) -> &'static str {
        match self {
            Pointer::Default => "default",
            Pointer::Hand => "pointer",
            Pointer::Text => "text",
            Pointer::ResizeEw => "ew-resize",
            Pointer::ResizeNs => "ns-resize",
        }
    }
}

/// The pointer shape for whatever a position hit. Divider hover is layered
/// on top by the caller, which knows the split geometry.
pub fn pointer_for(hit: Hit) -> Pointer {
    match hit {
        Hit::SidebarEntry(_)
        | Hit::SidebarNewAgent
        | Hit::SidebarViewGrid
        | Hit::SidebarViewSolo
        | Hit::PaneTitle(_)
        | Hit::PaneClose(_) => Pointer::Hand,
        Hit::Pane(_) => Pointer::Text,
        Hit::Sidebar | Hit::Status | Hit::Outside => Pointer::Default,
    }
}

/// Resolve what sits under (`x`, `y`) for a frame of `area`. With `zoomed`,
/// the solo pane owns the whole pane region and the tiled layout is
/// ignored — mirroring what `render` draws in solo view.
pub fn hit_test(
    area: Rect,
    session: &Session,
    side: SidebarSide,
    entries: &[SidebarEntry],
    zoomed: Option<PaneId>,
    x: u16,
    y: u16,
) -> Hit {
    if x < area.x || y < area.y || x >= area.x + area.width || y >= area.y + area.height {
        return Hit::Outside;
    }
    if area.height >= STATUS_HEIGHT && y >= area.y + area.height - STATUS_HEIGHT {
        return Hit::Status;
    }

    let panes = panes_area(area, side);
    let multi_pane = session.layout(panes.width, panes.height).len() > 1;

    let bar = sidebar_inner(area, side);
    if x >= bar.x && x < bar.x + bar.width && y >= bar.y && y < bar.y + bar.height {
        // Mirror render: the bottom rows belong to the pinned button, the
        // layout switcher (multi-pane only), and a breathing row — not to
        // agent cards.
        let mut cards = bar;
        if sidebar_button_row(area, side) == Some(y) {
            return Hit::SidebarNewAgent;
        }
        if sidebar_button_row(area, side).is_some() {
            cards.height = cards.height.saturating_sub(2);
        }
        if multi_pane && sidebar_view_row(area, side) == Some(y) {
            let (grid, solo) = view_toggle_cols();
            let col = x - bar.x;
            if grid.contains(&col) {
                return Hit::SidebarViewGrid;
            }
            if solo.contains(&col) {
                return Hit::SidebarViewSolo;
            }
            return Hit::Sidebar;
        }
        if multi_pane && sidebar_view_row(area, side).is_some() {
            cards.height = cards.height.saturating_sub(1);
        }
        if y >= cards.y + cards.height {
            return Hit::Sidebar;
        }
        return match sidebar_entry_at(cards, entries, session.window_count(), y) {
            Some(index) => Hit::SidebarEntry(index),
            None => Hit::Sidebar,
        };
    }

    if x >= panes.x && x < panes.x + panes.width && y >= panes.y && y < panes.y + panes.height {
        let local = local_panes(panes);
        let (lx, ly) = (x - panes.x, y - panes.y);
        let rects = match zoomed {
            Some(id) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
            None => session.layout(panes.width, panes.height),
        };
        for (id, rect) in rects {
            if lx >= rect.x && lx < rect.x + rect.width && ly >= rect.y && ly < rect.y + rect.height
            {
                return if rect.height >= 2 && ly == rect.y {
                    if close_button_cols(rect, local).is_some_and(|cols| cols.contains(&lx)) {
                        Hit::PaneClose(id)
                    } else {
                        Hit::PaneTitle(id)
                    }
                } else {
                    Hit::Pane(id)
                };
            }
        }
    }
    Hit::Sidebar
}

/// The sidebar entry whose card covers row `y`, mirroring the sidebar's
/// row layout: header + blank, then per entry an optional workspace header,
/// two card rows, and a blank spacer.
fn sidebar_entry_at(
    bar: Rect,
    entries: &[SidebarEntry],
    workspaces: usize,
    y: u16,
) -> Option<usize> {
    let mut row = bar.y + 2;
    let mut last_window: Option<usize> = None;
    for (index, entry) in entries.iter().enumerate() {
        if workspaces > 1 && last_window != Some(entry.window) {
            last_window = Some(entry.window);
            row += 1;
        }
        if y >= row && y < row + 2 {
            return Some(index);
        }
        row += 3;
        if row > bar.y + bar.height {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use roster_core::{AgentState, SplitDirection};
    use roster_detect::Detector;
    use std::time::Instant;

    fn setup() -> (Session, Vec<SidebarEntry>) {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("codex".into());
        session.set_reading(a, AgentState::Working, Some("w".into()), now);
        session.set_reading(b, AgentState::Blocked, Some("b".into()), now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        (session, entries)
    }

    #[test]
    fn regions_resolve_left_sidebar_layout() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        // 120 wide → sidebar 0..32 (rule at 31), panes 32..120, status row 29.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 0),
            Hit::Sidebar
        );
        // First card rows are 2 and 3 (header + blank above).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 2),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 3),
            Hit::SidebarEntry(0)
        );
        // Spacer row, then the second card.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 4),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 5),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 29),
            Hit::Status
        );
        // The pinned + new agent button owns the sidebar's bottom row; the
        // layout switcher the row above (two panes exist); breathing above
        // that is background.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 28),
            Hit::SidebarNewAgent
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 2, 27),
            Hit::SidebarViewGrid
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 9, 27),
            Hit::SidebarViewSolo
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 20, 27),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 26),
            Hit::Sidebar
        );
    }

    #[test]
    fn view_switcher_hides_with_a_single_pane() {
        let now = Instant::now();
        let mut session = Session::new();
        let only = session.focused().unwrap();
        session.pane_mut(only).unwrap().command = Some("claude".into());
        session.set_reading(only, AgentState::Idle, None, now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        let area = Rect::new(0, 0, 120, 30);
        // One pane: the switcher row is plain background.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 2, 27),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 9, 27),
            Hit::Sidebar
        );
    }

    #[test]
    fn pane_titles_and_content_resolve() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(88, 29);
        let (left_id, right_id) = (panes[0].0, panes[1].0);

        // Pane area starts at x=32. Row 0 is the title, rows below content.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 40, 0),
            Hit::PaneTitle(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 40, 10),
            Hit::Pane(left_id)
        );
        // Right half begins at local x 44 → absolute 76.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 80, 0),
            Hit::PaneTitle(right_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 80, 20),
            Hit::Pane(right_id)
        );
    }

    #[test]
    fn title_close_buttons_resolve_at_the_right_edge() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(88, 29);
        let (left_id, right_id) = (panes[0].0, panes[1].0);

        // Left pane: local rect 0..44, content width 43 (separator column),
        // so the ✕ target is local cols 40..43 → absolute 72..75.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 72, 0),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 74, 0),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 71, 0),
            Hit::PaneTitle(left_id)
        );
        // Below the title row the same columns are pane content.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 74, 5),
            Hit::Pane(left_id)
        );
        // Right pane: local rect 44..88, content width 44 (touches the
        // edge), ✕ target local 85..88 → absolute 117..120.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 118, 0),
            Hit::PaneClose(right_id)
        );
    }

    #[test]
    fn solo_view_maps_the_whole_pane_region_to_the_zoomed_pane() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(88, 29);
        let (left_id, right_id) = (panes[0].0, panes[1].0);

        // With the left pane solo, positions that belong to the right pane
        // in the grid all resolve to the solo pane.
        let zoomed = Some(left_id);
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, 80, 10),
            Hit::Pane(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, 80, 0),
            Hit::PaneTitle(left_id)
        );
        // Full-width title: content width 88 → ✕ at local 85..88 (absolute
        // 117..120).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, 118, 0),
            Hit::PaneClose(left_id)
        );
        // The sidebar still resolves normally, so cards switch panes.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, 5, 2),
            Hit::SidebarEntry(0)
        );
        let _ = right_id;
    }

    #[test]
    fn out_of_frame_is_outside() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 121, 5),
            Hit::Outside
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, 5, 30),
            Hit::Outside
        );
    }
}
