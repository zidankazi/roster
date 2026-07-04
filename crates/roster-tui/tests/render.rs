//! Full-frame snapshot test: panes on the left, sidebar on the right,
//! rendered through a real ratatui `Terminal` over the test backend.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use roster_core::{AgentState, Grid, Session, SplitDirection};
use roster_detect::Detector;
use roster_tui::{render, sidebar_entries, View, SIDEBAR_WIDTH};

fn buffer_row(buf: &Buffer, y: u16) -> String {
    let area = *buf.area();
    (area.x..area.right())
        .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn frame_lays_out_panes_and_sidebar() {
    let now = Instant::now();
    let mut session = Session::new();
    let left = session.focused().unwrap();
    let right = session.split(left, SplitDirection::Horizontal).unwrap();
    session.pane_mut(left).unwrap().command = Some("claude".into());
    session.pane_mut(right).unwrap().command = Some("codex".into());
    session.set_reading(
        left,
        AgentState::Working,
        Some("running tests".into()),
        now - Duration::from_secs(5),
    );
    session.set_reading(
        right,
        AgentState::Blocked,
        Some("Approve this command?".into()),
        now - Duration::from_secs(12),
    );

    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left agent output"));
    grids.insert(right, Grid::from_text("right agent output"));

    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let view = View {
        session: &session,
        grids: &grids,
        entries: &entries,
        selected: Some(0),
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    let completed = terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = completed.buffer.clone();

    // Pane area is 80 - 32 = 48 columns, split 24/24.
    let row0 = buffer_row(&buf, 0);
    assert!(row0.starts_with("left agent output"), "row0: {row0}");
    let right_pane: String = (24..48u16)
        .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string();
    assert_eq!(right_pane, "right agent output");

    // Sidebar occupies the last 32 columns: blocked row first, working next.
    let sidebar: Vec<String> = (0..2u16)
        .map(|y| {
            (80 - SIDEBAR_WIDTH..80)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert!(
        sidebar[0].starts_with("● codex blocked: Approve"),
        "sidebar row 0: {}",
        sidebar[0]
    );
    assert!(sidebar[0].ends_with("12s"), "sidebar row 0: {}", sidebar[0]);
    assert!(
        sidebar[1].starts_with("● claude-code working: run"),
        "sidebar row 1: {}",
        sidebar[1]
    );

    // Below the one-line grids, the pane region stays blank — content does
    // not bleed across rows or into the sidebar's columns.
    let pane_region_row1: String = (0..48u16)
        .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string();
    assert_eq!(pane_region_row1, "");
}
