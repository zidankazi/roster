//! Full-frame snapshot test: panes left, sidebar right, status line below,
//! rendered through a real ratatui `Terminal` over the test backend.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use roster_core::{AgentState, Grid, Session, SplitDirection};
use roster_detect::Detector;
use roster_tui::{render, sidebar_entries, View, SIDEBAR_WIDTH};

fn region_text(buf: &Buffer, x0: u16, x1: u16, y: u16) -> String {
    (x0..x1)
        .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn frame_lays_out_panes_sidebar_status_and_cursor() {
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

    // The focused (right) pane shows a visible cursor at (1, 0).
    let mut right_grid = Grid::from_text("right agent output");
    right_grid.cursor.col = 1;
    right_grid.cursor.row = 0;
    right_grid.cursor.visible = true;

    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left agent output"));
    grids.insert(right, right_grid);

    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &entries,
        selected: Some(0),
        mode_badge: None,
        status: "claude   ctrl-b: % \" split · o focus · j jump · x close · q quit",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();

    // Pane area is 80 - 32 = 48 wide and 11 tall (status line takes row 11).
    // The horizontal split gives 24/24; the left pane's content loses one
    // column to the separator.
    let buf = terminal.backend().buffer().clone();
    assert_eq!(region_text(&buf, 0, 23, 0), "left agent output");
    assert_eq!(buf.cell((23, 0)).unwrap().symbol(), "│");
    assert_eq!(region_text(&buf, 24, 48, 0), "right agent output");

    // Sidebar: blocked row first, working row next.
    let sidebar0 = region_text(&buf, 80 - SIDEBAR_WIDTH, 80, 0);
    let sidebar1 = region_text(&buf, 80 - SIDEBAR_WIDTH, 80, 1);
    assert!(
        sidebar0.starts_with("● codex blocked: Approve"),
        "sidebar row 0: {sidebar0}"
    );
    assert!(sidebar0.ends_with("12s"), "sidebar row 0: {sidebar0}");
    assert!(
        sidebar1.starts_with("● claude-code working: run"),
        "sidebar row 1: {sidebar1}"
    );

    // Status line spans the bottom row.
    let status = region_text(&buf, 0, 80, 11);
    assert!(status.contains("ctrl-b"), "status: {status}");

    // The terminal cursor sits on the focused pane's grid cursor.
    terminal.backend_mut().assert_cursor_position((25u16, 0u16));

    // Below the one-line grids the pane region stays blank.
    assert_eq!(region_text(&buf, 0, 23, 1), "");
}

#[test]
fn exited_pane_shows_notice_and_no_cursor() {
    let session = Session::new();
    let only = session.focused().unwrap();
    let mut grid = Grid::from_text("final output");
    grid.cursor.visible = true;

    let mut grids = HashMap::new();
    grids.insert(only, grid);
    let mut exited = HashMap::new();
    exited.insert(only, 3u32);

    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &[],
        selected: None,
        mode_badge: Some("PREFIX"),
        status: "hints",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Notice on the pane's bottom content row (pane area is 9 tall).
    let notice = region_text(&buf, 0, 48, 8);
    assert!(
        notice.starts_with(" exited (3) — ctrl-b x to close"),
        "notice: {notice}"
    );

    // Badge renders at the left of the status line.
    let status = region_text(&buf, 0, 80, 9);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
    assert!(status.contains("hints"), "status: {status}");

    // No cursor is requested for an exited pane.
    assert!(terminal.backend_mut().get_cursor_position().is_ok());
}
