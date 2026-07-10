//! Mouse hit-testing: map a click position to what it landed on.
//!
//! Mirrors the geometry `render` draws — the sidebar cards, pane titles and
//! content, and the status line — so the binary can translate mouse events
//! into the same intents keys produce.

use ratatui::layout::Rect;
use roster_core::{PaneId, Session};

use crate::sidebar::{auto_all_cols, auto_chip_cols, sidebar_rows, SidebarEntry, SidebarRow};
use crate::{
    close_button_cols, local_panes, panes_area, sidebar_button_row, sidebar_inner,
    status_view_spans, status_windows_span, SidebarSide, STATUS_HEIGHT,
};

/// What a screen position corresponds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A sidebar agent card.
    SidebarEntry(usize),
    /// An agent card's `auto` chip on its detail row — click toggles
    /// auto-approve for that pane.
    SidebarAuto(usize),
    /// The sidebar header's `auto-yes` fleet toggle — click arms
    /// auto-approve for every agent, or disarms all when everything is
    /// already on.
    SidebarAutoAll,
    /// The sidebar's pinned `+ new agent` button.
    SidebarNewAgent,
    /// The `grid` pill of the status row's layout switcher.
    StatusViewGrid,
    /// The `solo` pill of the status row's layout switcher.
    StatusViewSolo,
    /// Sidebar background (header, spacers, rule).
    Sidebar,
    /// A pane's title bar.
    PaneTitle(PaneId),
    /// A pane title's `✕` close button.
    PaneClose(PaneId),
    /// An exited pane's overlay `restart` button.
    PaneRestart(PaneId),
    /// A pane's content area (or its separator column).
    Pane(PaneId),
    /// The status line's workspace indicator — click cycles windows.
    StatusWindows,
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
        | Hit::SidebarAuto(_)
        | Hit::SidebarAutoAll
        | Hit::SidebarNewAgent
        | Hit::StatusViewGrid
        | Hit::StatusViewSolo
        | Hit::PaneTitle(_)
        | Hit::PaneClose(_)
        | Hit::PaneRestart(_)
        | Hit::StatusWindows => Pointer::Hand,
        // Pane content keeps the plain arrow: an I-beam over most of the
        // screen reads as noise, and selection works regardless.
        Hit::Pane(_) | Hit::Sidebar | Hit::Status | Hit::Outside => Pointer::Default,
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
    pos: (u16, u16),
) -> Hit {
    let (x, y) = pos;
    if x < area.x || y < area.y || x >= area.x + area.width || y >= area.y + area.height {
        return Hit::Outside;
    }
    let panes = panes_area(area, side);
    let multi_pane = session.layout(panes.width, panes.height).len() > 1;
    if area.height >= STATUS_HEIGHT && y >= area.y + area.height - STATUS_HEIGHT {
        let span = status_windows_span(
            area,
            session.active_window().unwrap_or(0),
            session.window_count(),
        );
        if let Some((rect, _)) = &span {
            if x >= rect.x && x < rect.x + rect.width {
                return Hit::StatusWindows;
            }
        }
        // The layout-switcher pills sit left of the workspace indicator —
        // mirroring draw_status.
        if let Some((grid, solo)) =
            status_view_spans(area, span.as_ref().map(|(rect, _)| rect), multi_pane)
        {
            if x >= grid.x && x < grid.x + grid.width {
                return Hit::StatusViewGrid;
            }
            if x >= solo.x && x < solo.x + solo.width {
                return Hit::StatusViewSolo;
            }
        }
        return Hit::Status;
    }

    let bar = sidebar_inner(area, side);
    if x >= bar.x && x < bar.x + bar.width && y >= bar.y && y < bar.y + bar.height {
        // Mirror render: the bottom rows belong to the pinned button and
        // its breathing row — not to agent cards.
        let mut cards = bar;
        if sidebar_button_row(area, side) == Some(y) {
            return Hit::SidebarNewAgent;
        }
        if sidebar_button_row(area, side).is_some() {
            cards.height = cards.height.saturating_sub(2);
        }
        if y >= cards.y + cards.height {
            return Hit::Sidebar;
        }
        // Mirror the sidebar's row plan: cards start two rows below the
        // sidebar's own header, and the header row hosts the `auto-yes`
        // fleet toggle.
        if y == bar.y {
            let on_button =
                auto_all_cols(bar.width).is_some_and(|cols| cols.contains(&(x - bar.x)));
            if on_button {
                return Hit::SidebarAutoAll;
            }
        }
        // Mirrors sidebar.rs's render: the sidebar's own header row plus one
        // blank spacer (`y += 2`) before the first card row.
        let first = cards.y + 2;
        if y < first {
            return Hit::Sidebar;
        }
        // The row plan needs the focused entry — only its card grows the
        // full telemetry row — resolved exactly the way render does.
        let focused = session
            .focused()
            .and_then(|id| entries.iter().position(|entry| entry.pane == id));
        let rows = sidebar_rows(entries, focused);
        return match rows.get(usize::from(y - first)) {
            Some(SidebarRow::EntryName(index)) => Hit::SidebarEntry(*index),
            Some(SidebarRow::EntryDetail(index)) => {
                // The `auto` chip's columns toggle auto-approve; the rest
                // of the row is the card, same as the name row above it.
                let on_chip =
                    auto_chip_cols(bar.width).is_some_and(|cols| cols.contains(&(x - bar.x)));
                if on_chip {
                    Hit::SidebarAuto(*index)
                } else {
                    Hit::SidebarEntry(*index)
                }
            }
            // Badges are informational; the click is the card, so the whole
            // taller card stays one jump target.
            Some(SidebarRow::EntryTelemetry(index)) => Hit::SidebarEntry(*index),
            Some(SidebarRow::Blank) | None => Hit::Sidebar,
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
        session.pane_mut(b).unwrap().command = Some("claude".into());
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
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 0)),
            Hit::Sidebar
        );
        // First card rows are 2 and 3 (header + blank above).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 2)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 3)),
            Hit::SidebarEntry(0)
        );
        // Spacer row, then the second card.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 4)),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 5)),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 29)),
            Hit::Status
        );
        // The pinned + new agent button owns the sidebar's bottom row;
        // breathing above it is background (the layout switcher lives on
        // the status row now).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 28)),
            Hit::SidebarNewAgent
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (2, 27)),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (9, 27)),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 26)),
            Hit::Sidebar
        );
    }

    #[test]
    fn clicks_resolve_to_the_triage_ordered_cards() {
        // Creation order: pane a (working) then pane b (idle). Triage puts
        // the idle card first, and the click follows the reordered rows —
        // the top card jumps to pane b, not to the first-created pane.
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Working, Some("w".into()), now);
        session.set_reading(b, AgentState::Idle, None, now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);

        let area = Rect::new(0, 0, 120, 30);
        let first = hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 2));
        assert_eq!(first, Hit::SidebarEntry(0));
        assert_eq!(entries[0].pane, b);
        let second = hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 5));
        assert_eq!(second, Hit::SidebarEntry(1));
        assert_eq!(entries[1].pane, a);
    }

    #[test]
    fn auto_chip_cols_resolve_to_auto_hits_on_detail_rows_only() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        // The header row hosts the auto-yes fleet toggle at cols 20..30;
        // the rest of the header is inert sidebar.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (20, 0)),
            Hit::SidebarAutoAll
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (29, 0)),
            Hit::SidebarAutoAll
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (30, 0)),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (19, 0)),
            Hit::Sidebar
        );

        // Sidebar inner is 31 wide, so every card's chip spans cols 24..30
        // of its detail row — rows 3 and 6.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (24, 3)),
            Hit::SidebarAuto(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (29, 3)),
            Hit::SidebarAuto(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (27, 6)),
            Hit::SidebarAuto(1)
        );
        // Off the chip — before it, past it, or the name row above — the
        // click is the card.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (23, 3)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (30, 3)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (26, 2)),
            Hit::SidebarEntry(0)
        );
    }

    #[test]
    fn telemetry_rows_resolve_to_their_card_and_shift_the_rows_below() {
        let now = Instant::now();
        let (mut session, entries) = setup();
        // Feed telemetry to the blocked pane — the first card, and the
        // focused pane (split moves focus to the new pane), so its card
        // grows the full third row and everything below shifts down one.
        let blocked = entries[0].pane;
        assert_eq!(session.focused(), Some(blocked));
        session.set_telemetry(
            blocked,
            Some(roster_core::Telemetry {
                model: Some("Opus".into()),
                ..roster_core::Telemetry::default()
            }),
        );
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        let area = Rect::new(0, 0, 120, 30);
        // Card 0 spans rows 2-4 (name, detail, telemetry); the badge row is
        // the card, not a chip — clicking it jumps.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 4)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (27, 4)),
            Hit::SidebarEntry(0),
            "the chip columns on a telemetry row are still the card"
        );
        // The second card sits a row lower than the two-line layout put it —
        // its chip included. (The full row plan is sidebar.rs's test.)
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 6)),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (27, 7)),
            Hit::SidebarAuto(1)
        );
    }

    #[test]
    fn view_switcher_pills_sit_on_the_status_row_and_hide_with_a_single_pane() {
        let now = Instant::now();
        let mut session = Session::new();
        let only = session.focused().unwrap();
        session.pane_mut(only).unwrap().command = Some("claude".into());
        session.set_reading(only, AgentState::Idle, None, now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        let area = Rect::new(0, 0, 120, 30);
        // One pane: no pills — the whole status row is plain status.
        for x in [100, 108, 115] {
            assert_eq!(
                hit_test(area, &session, SidebarSide::Left, &entries, None, (x, 29)),
                Hit::Status
            );
        }

        // A second pane brings the pills: with a single window (no ⧉
        // indicator) they end one column in from the right edge — solo at
        // 113..119, grid at 106..112.
        let (session, entries) = setup();
        for (x, hit) in [
            (105, Hit::Status),
            (106, Hit::StatusViewGrid),
            (111, Hit::StatusViewGrid),
            (112, Hit::Status),
            (113, Hit::StatusViewSolo),
            (118, Hit::StatusViewSolo),
            (119, Hit::Status),
        ] {
            assert_eq!(
                hit_test(area, &session, SidebarSide::Left, &entries, None, (x, 29)),
                hit,
                "at x={x}"
            );
        }
    }

    #[test]
    fn flat_clicks_follow_the_global_order_across_workspaces() {
        let now = Instant::now();
        // Two workspaces, one pane each: window 0 idle, window 1 blocked.
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Blocked, Some("q".into()), now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        let area = Rect::new(0, 0, 120, 30);

        // The flat plan has no workspace headers, so the top card row (2)
        // is the globally top-ranked agent — the blocked one in window 1.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 2)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(entries[0].pane, b);

        // The removed triage switcher's old row resolves as inert sidebar
        // background, not a phantom toggle.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (3, 27)),
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
            hit_test(area, &session, SidebarSide::Left, &entries, None, (40, 0)),
            Hit::PaneTitle(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (40, 10)),
            Hit::Pane(left_id)
        );
        // Right half begins at local x 44 → absolute 76.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (80, 0)),
            Hit::PaneTitle(right_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (80, 20)),
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
            hit_test(area, &session, SidebarSide::Left, &entries, None, (72, 0)),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (74, 0)),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (71, 0)),
            Hit::PaneTitle(left_id)
        );
        // Below the title row the same columns are pane content.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (74, 5)),
            Hit::Pane(left_id)
        );
        // Right pane: local rect 44..88, content width 44 (touches the
        // edge), ✕ target local 85..88 → absolute 117..120.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (118, 0)),
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
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                zoomed,
                (80, 10)
            ),
            Hit::Pane(left_id)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, (80, 0)),
            Hit::PaneTitle(left_id)
        );
        // Full-width title: content width 88 → ✕ at local 85..88 (absolute
        // 117..120).
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                zoomed,
                (118, 0)
            ),
            Hit::PaneClose(left_id)
        );
        // The sidebar still resolves normally, so cards switch panes.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, zoomed, (5, 2)),
            Hit::SidebarEntry(0)
        );
        let _ = right_id;
    }

    #[test]
    fn agentless_windows_leave_no_sidebar_rows_in_the_flat_plan() {
        let now = Instant::now();
        let mut session = Session::new();
        // Window 0: a plain shell, no agents — it contributes nothing to
        // the flat list. Window 1: a single agent, whose card leads.
        let shell = session.focused().unwrap();
        session.pane_mut(shell).unwrap().command = Some("zsh".into());
        let agent = session.new_window();
        session.pane_mut(agent).unwrap().command = Some("claude".into());
        session.set_reading(agent, AgentState::Working, Some("w".into()), now);
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);

        let area = Rect::new(0, 0, 120, 30);
        // Rows: the agent's card at y2-3, then blank — no header or
        // placeholder rows for the agentless window.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 2)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 3)),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 4)),
            Hit::Sidebar
        );
    }

    #[test]
    fn status_indicator_resolves_only_with_multiple_windows() {
        let (mut session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        // One window: no ⧉ indicator — its columns fall to the solo pill
        // (two panes exist) and plain status beyond it.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (115, 29)),
            Hit::StatusViewSolo
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (119, 29)),
            Hit::Status
        );
        session.new_window();
        // Two windows: `⧉ 1/2` plus padding is 7 columns at the right
        // edge. The new window is active and holds a single pane, so the
        // pills hide with it — layouts to switch between are the active
        // window's.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (115, 29)),
            Hit::StatusWindows
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (108, 29)),
            Hit::Status
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (60, 29)),
            Hit::Status
        );
    }

    #[test]
    fn out_of_frame_is_outside() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (121, 5)),
            Hit::Outside
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, None, (5, 30)),
            Hit::Outside
        );
    }
}
