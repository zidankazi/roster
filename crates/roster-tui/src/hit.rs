//! Mouse hit-testing: map a click position to what it landed on.
//!
//! Mirrors the geometry `render` draws — the sidebar cards, pane titles and
//! content, and the status line — so the binary can translate mouse events
//! into the same intents keys produce.

use ratatui::layout::Rect;
use roster_core::{PaneId, Session};

use crate::sidebar::SidebarEntry;
use crate::{content_rect, local_panes, panes_area, sidebar_inner, SidebarSide, STATUS_HEIGHT};

/// What a screen position corresponds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A sidebar agent card.
    SidebarEntry(usize),
    /// Sidebar background (header, spacers, rule).
    Sidebar,
    /// A pane's title bar.
    PaneTitle(PaneId),
    /// A pane's content area (or its separator column).
    Pane(PaneId),
    /// The status line.
    Status,
    /// Nothing interactive.
    Outside,
}

/// Resolve what sits under (`x`, `y`) for a frame of `area`.
pub fn hit_test(
    area: Rect,
    session: &Session,
    side: SidebarSide,
    entries: &[SidebarEntry],
    x: u16,
    y: u16,
) -> Hit {
    if x < area.x || y < area.y || x >= area.x + area.width || y >= area.y + area.height {
        return Hit::Outside;
    }
    if area.height >= STATUS_HEIGHT && y >= area.y + area.height - STATUS_HEIGHT {
        return Hit::Status;
    }

    let bar = sidebar_inner(area, side);
    if x >= bar.x && x < bar.x + bar.width && y >= bar.y && y < bar.y + bar.height {
        return match sidebar_entry_at(bar, entries, session.window_count(), y) {
            Some(index) => Hit::SidebarEntry(index),
            None => Hit::Sidebar,
        };
    }

    let panes = panes_area(area, side);
    if x >= panes.x && x < panes.x + panes.width && y >= panes.y && y < panes.y + panes.height {
        let local = local_panes(panes);
        let (lx, ly) = (x - panes.x, y - panes.y);
        for (id, rect) in session.layout(panes.width, panes.height) {
            if lx >= rect.x && lx < rect.x + rect.width && ly >= rect.y && ly < rect.y + rect.height
            {
                let content = content_rect(rect, local);
                return if rect.height >= 2 && ly == rect.y {
                    Hit::PaneTitle(id)
                } else {
                    let _ = content;
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
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 0),
            Hit::Sidebar
        );
        // First card rows are 2 and 3 (header + blank above).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 2),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 3),
            Hit::SidebarEntry(0)
        );
        // Spacer row, then the second card.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 4),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 5),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 29),
            Hit::Status
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
            hit_test(area, &session, SidebarSide::Left, &entries, 40, 0),
            Hit::PaneTitle(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 40, 10),
            Hit::Pane(left_id)
        );
        // Right half begins at local x 44 → absolute 76.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 80, 0),
            Hit::PaneTitle(right_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 80, 20),
            Hit::Pane(right_id)
        );
    }

    #[test]
    fn out_of_frame_is_outside() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 121, 5),
            Hit::Outside
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, 5, 30),
            Hit::Outside
        );
    }
}
