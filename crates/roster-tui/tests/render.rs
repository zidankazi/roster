//! Full-frame snapshot test: sidebar on one edge, panes on the other,
//! status line below, rendered through a real ratatui `Terminal`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use roster_core::{AgentState, Grid, Session, SplitDirection};
use roster_detect::Detector;
use roster_tui::{render, sidebar_entries, SidebarSide, View};

fn region_text(buf: &Buffer, x0: u16, x1: u16, y: u16) -> String {
    (x0..x1)
        .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn two_agent_session(now: Instant) -> (Session, roster_core::PaneId, roster_core::PaneId) {
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
    (session, left, right)
}

#[test]
fn left_sidebar_places_panes_to_the_right_with_cursor() {
    let now = Instant::now();
    let (mut session, _left, right) = two_agent_session(now);
    session.focus(right);

    // Focused (right) pane shows a visible cursor at grid (1, 0).
    let mut right_grid = Grid::from_text("right agent output");
    right_grid.cursor.col = 1;
    right_grid.cursor.row = 0;
    right_grid.cursor.visible = true;
    let mut grids = HashMap::new();
    grids.insert(_left, Grid::from_text("left agent output"));
    grids.insert(right, right_grid);

    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &entries,
        selected: None,
        side: SidebarSide::Left,
        mode_badge: None,
        status: "codex   ctrl-b: % \" split · o focus · j jump · x close · q quit",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Sidebar occupies the left 32 columns: title, rule, then the blocked
    // agent's card floated to the top.
    assert!(region_text(&buf, 0, 32, 0).starts_with(" roster"));
    assert!(region_text(&buf, 0, 32, 1).starts_with("──"));
    let card = region_text(&buf, 0, 32, 2);
    assert!(card.trim_start().starts_with("● codex"), "card: {card}");
    assert!(card.ends_with("12s"), "card: {card}");
    assert!(region_text(&buf, 0, 32, 3)
        .trim_start()
        .starts_with("blocked · Approve"));

    // Panes occupy columns 32..80 (two halves of 24). The pane area is 48
    // wide; the left half loses a column to the separator.
    assert_eq!(region_text(&buf, 32, 55, 0), "left agent output");
    assert_eq!(region_text(&buf, 56, 80, 0), "right agent output");

    // Cursor sits on the focused pane's grid cursor, offset into the pane
    // region: panes start at x=32, right half at 32+24=56, grid col 1 → 57.
    terminal.backend_mut().assert_cursor_position((57u16, 0u16));

    // Status line spans the bottom row.
    assert!(region_text(&buf, 0, 80, 11).contains("ctrl-b"));
}

#[test]
fn right_sidebar_places_panes_on_the_left() {
    let now = Instant::now();
    let (session, left, right) = two_agent_session(now);
    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left agent output"));
    grids.insert(right, Grid::from_text("right agent output"));

    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &entries,
        selected: None,
        side: SidebarSide::Right,
        mode_badge: None,
        status: "status",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Panes on the left starting at column 0; sidebar title in the right 32.
    assert_eq!(region_text(&buf, 0, 23, 0), "left agent output");
    assert!(region_text(&buf, 48, 80, 0).starts_with(" roster"));
}

#[test]
fn exited_pane_shows_notice() {
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
        side: SidebarSide::Left,
        mode_badge: Some("PREFIX"),
        status: "hints",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Notice on the pane's bottom content row (pane area is 9 tall, panes
    // start at column 32 with a left sidebar).
    let notice = region_text(&buf, 32, 80, 8);
    assert!(
        notice.starts_with(" exited (3) — ctrl-b x to close"),
        "notice: {notice}"
    );

    // Badge renders at the left of the status line.
    let status = region_text(&buf, 0, 80, 9);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
    assert!(status.contains("hints"), "status: {status}");
}
