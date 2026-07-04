//! ratatui rendering: pane contents and the agent-state sidebar.
//!
//! Renders a known model into a ratatui buffer — deterministic and
//! snapshot-testable. Input is surfaced as intent [`Message`]s; the binary
//! owns the side effects. See `docs/01-crates.md`.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::Frame;
use roster_core::{Grid, PaneId, Session};

mod pane;
mod sidebar;
mod style;

pub use pane::PaneView;
pub use sidebar::{
    format_age, sidebar_entries, Message, Sidebar, SidebarEntry, SidebarState,
};
pub use style::{cell_style, state_color, state_label};

/// Columns reserved for the sidebar, when the terminal is wide enough to
/// afford them; narrower terminals give it up to half the width.
pub const SIDEBAR_WIDTH: u16 = 32;

/// Everything one frame needs: the model, each pane's screen, and the
/// prepared sidebar rows.
pub struct View<'a> {
    /// The session being displayed.
    pub session: &'a Session,
    /// Each pane's current screen grid. Panes without one render blank.
    pub grids: &'a HashMap<PaneId, Grid>,
    /// Sidebar rows, already built and sorted (see [`sidebar_entries`]).
    pub entries: &'a [SidebarEntry],
    /// The sidebar row to highlight, if any.
    pub selected: Option<usize>,
}

/// Draw one frame: the active window's panes on the left, the sidebar on
/// the right.
pub fn render(frame: &mut Frame, view: &View) {
    let area = frame.area();
    let sidebar_width = SIDEBAR_WIDTH.min(area.width / 2);
    let panes_width = area.width - sidebar_width;

    for (id, rect) in view.session.layout(panes_width, area.height) {
        let Some(grid) = view.grids.get(&id) else {
            continue;
        };
        let target = Rect::new(
            area.x + rect.x,
            area.y + rect.y,
            rect.width,
            rect.height,
        );
        frame.render_widget(PaneView::new(grid), target);
    }

    let sidebar_area = Rect::new(area.x + panes_width, area.y, sidebar_width, area.height);
    frame.render_widget(Sidebar::new(view.entries, view.selected), sidebar_area);
}
