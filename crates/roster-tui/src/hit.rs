//! Mouse hit-testing: map a click position to what it landed on.
//!
//! Mirrors the geometry `render` draws — the sidebar cards, pane titles and
//! content, and the status line — so the binary can translate mouse events
//! into the same intents keys produce.

use ratatui::layout::Rect;
use roster_core::{PaneId, Session};

use crate::sidebar::{
    auto_all_cols, auto_chip_cols, focused_entry, limits_footer_height, sidebar_rows, SidebarEntry,
    SidebarRow,
};
use crate::{
    chrome_area, close_button_cols, panes_area, sidebar_button_row, sidebar_inner, status_controls,
    SidebarSide, StatusControls, STATUS_HEIGHT,
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
    /// The workspace row of the title banner — hovering it reveals the
    /// full path on the divider row below when the shown path was cut to
    /// fit.
    SidebarWorkspace,
    /// The `grid` pill of the status row's layout switcher.
    StatusViewGrid,
    /// The `solo` pill of the status row's layout switcher.
    StatusViewSolo,
    /// Sidebar background (header, spacers, the gap column).
    Sidebar,
    /// A pane's title bar.
    PaneTitle(PaneId),
    /// A pane title's `✕` close button.
    PaneClose(PaneId),
    /// An exited pane's overlay `restart` button.
    PaneRestart(PaneId),
    /// A pane's content area (or its panel's side and bottom borders).
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
        // screen reads as noise, and selection works regardless. The
        // workspace row isn't clickable either — its hover is a tooltip
        // reveal, not an affordance that wants a hand.
        Hit::Pane(_) | Hit::Sidebar | Hit::Status | Hit::Outside | Hit::SidebarWorkspace => {
            Pointer::Default
        }
    }
}

/// The parts of what `render` drew that hit-testing must mirror but that
/// don't fit the layout math alone: everything here is a `render` input
/// that shifts or shortens the card region, grouped so `hit_test` stays
/// under clippy's argument-count gate as the sidebar chrome grows.
#[derive(Clone, Copy)]
pub struct HitContext<'a> {
    /// The fleet rate-limit reading render was given: its footer shortens
    /// the card region, and the two sides disagreeing would land footer
    /// clicks on cards the shrunken region no longer shows.
    pub limits: Option<&'a roster_core::RateLimit>,
    /// The solo-view pane, if any: it owns the whole pane region and the
    /// tiled layout is ignored, mirroring what `render` draws in solo view.
    pub zoomed: Option<PaneId>,
    /// Whether render drew the three-row title/workspace banner (title,
    /// workspace + clock, divider) above the `agents` row — present, it
    /// pushes every row below it (the `auto-yes` toggle, the first card)
    /// down by three.
    pub workspace_header: bool,
}

/// Resolve what sits under (`x`, `y`) for a frame of `area`. `context`
/// carries the render inputs hit-testing must mirror (see [`HitContext`]).
pub fn hit_test(
    area: Rect,
    session: &Session,
    side: SidebarSide,
    entries: &[SidebarEntry],
    context: &HitContext,
    pos: (u16, u16),
) -> Hit {
    let HitContext {
        limits,
        zoomed,
        workspace_header,
    } = *context;
    let (x, y) = pos;
    // Positions in the inset margin are outside the chrome — nothing
    // interactive lives on the bare canvas.
    let inner = chrome_area(area);
    if x < inner.x || y < inner.y || x >= inner.x + inner.width || y >= inner.y + inner.height {
        return Hit::Outside;
    }
    if inner.height >= STATUS_HEIGHT && y >= inner.y + inner.height - STATUS_HEIGHT {
        // One resolver shared with draw_status, so the targets can't
        // drift off the drawn controls.
        let StatusControls {
            windows: span,
            views,
        } = status_controls(area, side, session);
        if let Some((rect, _)) = &span {
            if x >= rect.x && x < rect.x + rect.width {
                return Hit::StatusWindows;
            }
        }
        if let Some((grid, solo)) = views {
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
        // The fleet rate-limit footer shortens the card region exactly as
        // render reserves it; its rows are informational, not clickable.
        cards.height = cards
            .height
            .saturating_sub(limits_footer_height(limits, cards.height));
        if y >= cards.y + cards.height {
            return Hit::Sidebar;
        }
        // The title/workspace banner, when render drew it, sits above the
        // `agents` row — everything below shifts down by its three rows.
        // Its second row (the path) is the one hoverable target inside it;
        // the title and divider rows are inert.
        if workspace_header && y == bar.y + 1 {
            return Hit::SidebarWorkspace;
        }
        let header = bar.y + u16::from(workspace_header) * 3;
        // Mirror the sidebar's row plan: cards start two rows below the
        // sidebar's own header, and the header row hosts the `auto-yes`
        // fleet toggle.
        if y == header {
            let on_button =
                auto_all_cols(bar.width).is_some_and(|cols| cols.contains(&(x - bar.x)));
            if on_button {
                return Hit::SidebarAutoAll;
            }
        }
        // Mirrors sidebar.rs's render: the sidebar's own header row plus one
        // blank spacer (`y += 2`) before the first card row.
        let first = header + 2;
        if y < first {
            return Hit::Sidebar;
        }
        // The row plan needs the focused entry — only its card grows the
        // full telemetry row — resolved by the same helper render uses.
        let rows = sidebar_rows(entries, focused_entry(entries, session.focused()));
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

    let panes = panes_area(area, side);
    if x >= panes.x && x < panes.x + panes.width && y >= panes.y && y < panes.y + panes.height {
        let (lx, ly) = (x - panes.x, y - panes.y);
        let rects = match zoomed {
            Some(id) => vec![(id, roster_core::Rect::new(0, 0, panes.width, panes.height))],
            None => session.layout(panes.width, panes.height),
        };
        for (id, rect) in rects {
            if lx >= rect.x && lx < rect.x + rect.width && ly >= rect.y && ly < rect.y + rect.height
            {
                // The panel's top border is the title (with its ✕ target);
                // the side and bottom borders are the pane, like content —
                // a click anywhere in the panel focuses it.
                return if crate::panelled(rect) && ly == rect.y {
                    if close_button_cols(rect).is_some_and(|cols| cols.contains(&lx)) {
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
        // Chrome inset (2,1) → sidebar 2..34, panes 34..118, status row 28;
        // the margin itself is dead.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 0)
            ),
            Hit::Outside
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (0, 5)
            ),
            Hit::Outside
        );
        // The sidebar's own header row is inert background.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 1)
            ),
            Hit::Sidebar
        );
        // First card rows are 3 and 4 (header + blank above).
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 3)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 4)
            ),
            Hit::SidebarEntry(0)
        );
        // Spacer row, then the second card.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 5)
            ),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 6)
            ),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 28)
            ),
            Hit::Status
        );
        // The bottom margin row is outside the chrome.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 29)
            ),
            Hit::Outside
        );
        // The pinned + new agent button owns the sidebar's bottom row;
        // breathing above it is background (the layout switcher lives on
        // the status row now).
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 27)
            ),
            Hit::SidebarNewAgent
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (4, 26)
            ),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (11, 26)
            ),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 25)
            ),
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
        let first = hit_test(
            area,
            &session,
            SidebarSide::Left,
            &entries,
            &HitContext {
                limits: None,
                zoomed: None,
                workspace_header: false,
            },
            (5, 3),
        );
        assert_eq!(first, Hit::SidebarEntry(0));
        assert_eq!(entries[0].pane, b);
        let second = hit_test(
            area,
            &session,
            SidebarSide::Left,
            &entries,
            &HitContext {
                limits: None,
                zoomed: None,
                workspace_header: false,
            },
            (5, 6),
        );
        assert_eq!(second, Hit::SidebarEntry(1));
        assert_eq!(entries[1].pane, a);
    }

    #[test]
    fn auto_chip_cols_resolve_to_auto_hits_on_detail_rows_only() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        // The header row (y=1, inside the inset) hosts the auto-yes fleet
        // toggle at inner cols 20..30 → absolute 22..32; the rest of the
        // header is inert sidebar.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (22, 1)
            ),
            Hit::SidebarAutoAll
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (31, 1)
            ),
            Hit::SidebarAutoAll
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (32, 1)
            ),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (21, 1)
            ),
            Hit::Sidebar
        );

        // Sidebar inner is 31 wide, so every card's chip spans inner cols
        // 24..30 → absolute 26..32 of its detail row — rows 4 and 7.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (26, 4)
            ),
            Hit::SidebarAuto(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (31, 4)
            ),
            Hit::SidebarAuto(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (29, 7)
            ),
            Hit::SidebarAuto(1)
        );
        // Off the chip — before it, past it, or the name row above — the
        // click is the card.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (25, 4)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (32, 4)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (28, 3)
            ),
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
        // Card 0 spans rows 3-5 (name, detail, telemetry); the badge row is
        // the card, not a chip — clicking it jumps.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 5)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (29, 5)
            ),
            Hit::SidebarEntry(0),
            "the chip columns on a telemetry row are still the card"
        );
        // The second card sits a row lower than the two-line layout put it —
        // its chip included. (The full row plan is sidebar.rs's test.)
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 7)
            ),
            Hit::SidebarEntry(1)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (29, 8)
            ),
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
        // One pane: no pills — the whole status row (y=28, inside the
        // inset) is plain status.
        for x in [100, 108, 115] {
            assert_eq!(
                hit_test(
                    area,
                    &session,
                    SidebarSide::Left,
                    &entries,
                    &HitContext {
                        limits: None,
                        zoomed: None,
                        workspace_header: false
                    },
                    (x, 28)
                ),
                Hit::Status
            );
        }

        // A second pane brings the pills: with a single window (no ⧉
        // indicator) they end one column in from the chrome's right edge
        // (118) — solo at 111..117, grid at 104..110.
        let (session, entries) = setup();
        for (x, hit) in [
            (103, Hit::Status),
            (104, Hit::StatusViewGrid),
            (109, Hit::StatusViewGrid),
            (110, Hit::Status),
            (111, Hit::StatusViewSolo),
            (116, Hit::StatusViewSolo),
            (117, Hit::Status),
        ] {
            assert_eq!(
                hit_test(
                    area,
                    &session,
                    SidebarSide::Left,
                    &entries,
                    &HitContext {
                        limits: None,
                        zoomed: None,
                        workspace_header: false
                    },
                    (x, 28)
                ),
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

        // The flat plan has no workspace headers, so the top card row (3)
        // is the globally top-ranked agent — the blocked one in window 1.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 3)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(entries[0].pane, b);

        // The removed triage switcher's old row resolves as inert sidebar
        // background, not a phantom toggle.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (3, 26)
            ),
            Hit::Sidebar
        );
    }

    #[test]
    fn pane_titles_and_content_resolve() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(84, 27);
        let (left_id, right_id) = (panes[0].0, panes[1].0);

        // Pane area starts at x=34, y=1 (chrome inset). The panel's top
        // border row is the title, rows below are content.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (40, 1)
            ),
            Hit::PaneTitle(left_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (40, 10)
            ),
            Hit::Pane(left_id)
        );
        // Right half begins at local x 42 → absolute 76.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (80, 1)
            ),
            Hit::PaneTitle(right_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (80, 20)
            ),
            Hit::Pane(right_id)
        );
        // Side and bottom borders are the pane — a click anywhere in the
        // panel focuses it.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (34, 10)
            ),
            Hit::Pane(left_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (40, 27)
            ),
            Hit::Pane(left_id)
        );
    }

    #[test]
    fn title_close_buttons_resolve_at_the_right_edge() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(84, 27);
        let (left_id, right_id) = (panes[0].0, panes[1].0);

        // Left pane: local rect 0..42, ✕ target one column in from the
        // panel corner — local cols 38..41 → absolute 72..75.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (72, 1)
            ),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (74, 1)
            ),
            Hit::PaneClose(left_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (71, 1)
            ),
            Hit::PaneTitle(left_id)
        );
        // The corner itself is the title, not the ✕.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (75, 1)
            ),
            Hit::PaneTitle(left_id)
        );
        // Below the title row the same columns are pane content.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (74, 5)
            ),
            Hit::Pane(left_id)
        );
        // Right pane: local rect 42..84, ✕ target local 80..83 →
        // absolute 114..117.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (115, 1)
            ),
            Hit::PaneClose(right_id)
        );
    }

    #[test]
    fn solo_view_maps_the_whole_pane_region_to_the_zoomed_pane() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let panes = session.layout(84, 27);
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
                &HitContext {
                    limits: None,
                    zoomed,
                    workspace_header: false
                },
                (80, 10)
            ),
            Hit::Pane(left_id)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed,
                    workspace_header: false
                },
                (80, 1)
            ),
            Hit::PaneTitle(left_id)
        );
        // Full-width panel: 84 wide → ✕ at local 80..83 (absolute
        // 114..117).
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed,
                    workspace_header: false
                },
                (115, 1)
            ),
            Hit::PaneClose(left_id)
        );
        // The sidebar still resolves normally, so cards switch panes.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed,
                    workspace_header: false
                },
                (5, 3)
            ),
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
        // Rows: the agent's card at y3-4, then blank — no header or
        // placeholder rows for the agentless window.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 3)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 4)
            ),
            Hit::SidebarEntry(0)
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 5)
            ),
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
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (115, 28)
            ),
            Hit::StatusViewSolo
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (117, 28)
            ),
            Hit::Status
        );
        session.new_window();
        // Two windows: `⧉ 1/2` plus padding is 7 columns at the chrome's
        // right edge (111..118). The new window is active and holds a
        // single pane, so the pills hide with it — layouts to switch
        // between are the active window's.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (115, 28)
            ),
            Hit::StatusWindows
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (108, 28)
            ),
            Hit::Status
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (60, 28)
            ),
            Hit::Status
        );
    }

    #[test]
    fn footer_rows_are_inert_and_never_phantom_cards() {
        // Enough agents that the row plan reaches past the footer's rows:
        // without the limits-aware shrink, a click on the footer would
        // resolve to a card render no longer draws there.
        let now = Instant::now();
        let mut session = Session::new();
        let mut target = session.focused().unwrap();
        for _ in 0..7 {
            target = session.split(target, SplitDirection::Horizontal).unwrap();
        }
        let ids: Vec<PaneId> = session.panes().into_iter().map(|pane| pane.id).collect();
        for id in ids {
            session.pane_mut(id).unwrap().command = Some("claude".into());
            session.set_reading(id, AgentState::Idle, None, now);
        }
        let entries = crate::sidebar_entries(&session, &Detector::builtin(), now);
        assert_eq!(entries.len(), 8);
        let limits = roster_core::RateLimit {
            five_hour: Some(roster_core::RateLimitWindow {
                used_pct: 62.0,
                resets_in: None,
            }),
            seven_day: Some(roster_core::RateLimitWindow {
                used_pct: 41.0,
                resets_in: None,
            }),
        };
        let area = Rect::new(0, 0, 120, 30);
        // Footer-less, row 24 is the eighth card's name row…
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 24)
            ),
            Hit::SidebarEntry(7)
        );
        // …with the footer reserved (3 rows off the card region), the same
        // click is inert sidebar background, mirroring what render shows.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: Some(&limits),
                    zoomed: None,
                    workspace_header: false
                },
                (5, 24)
            ),
            Hit::Sidebar
        );
        // Cards above the footer keep their targets.
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: Some(&limits),
                    zoomed: None,
                    workspace_header: false
                },
                (5, 21)
            ),
            Hit::SidebarEntry(6)
        );
    }

    #[test]
    fn workspace_banner_rows_resolve_and_shift_everything_below_by_three() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        let ctx = HitContext {
            limits: None,
            zoomed: None,
            workspace_header: true,
        };
        // Title row (y=1, inset) is inert; the path row (y=2) is the
        // hoverable target; the divider (y=3) is inert too.
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, &ctx, (5, 1)),
            Hit::Sidebar
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, &ctx, (5, 2)),
            Hit::SidebarWorkspace
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, &ctx, (5, 3)),
            Hit::Sidebar
        );
        // The `agents` header and the first card each land three rows
        // later than the no-banner layout (rows 1 and 3 there).
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, &ctx, (22, 4)),
            Hit::SidebarAutoAll
        );
        assert_eq!(
            hit_test(area, &session, SidebarSide::Left, &entries, &ctx, (5, 6)),
            Hit::SidebarEntry(0)
        );
    }

    #[test]
    fn out_of_frame_is_outside() {
        let (session, entries) = setup();
        let area = Rect::new(0, 0, 120, 30);
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (121, 5)
            ),
            Hit::Outside
        );
        assert_eq!(
            hit_test(
                area,
                &session,
                SidebarSide::Left,
                &entries,
                &HitContext {
                    limits: None,
                    zoomed: None,
                    workspace_header: false
                },
                (5, 30)
            ),
            Hit::Outside
        );
    }
}
