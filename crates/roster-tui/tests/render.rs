//! Full-frame snapshot test: sidebar on one edge, titled panes on the
//! other, status line below, rendered through a real ratatui `Terminal`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::Terminal;
use roster_core::{AgentState, Grid, Session, SplitDirection};
use roster_detect::Detector;
use roster_tui::{launch_items, render, sidebar_entries, LauncherState, SidebarSide, View};

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
fn panes_get_title_bars_and_content_shifts_down() {
    let now = Instant::now();
    let (mut session, left, right) = two_agent_session(now);
    session.focus(right);

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
        selected: None,
        side: SidebarSide::Left,
        launcher: None,
        mode_badge: None,
        status: "hints with ctrl-b",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Row 0 of the pane region holds title bars, not content: the working
    // claude pane's title on the left half, blocked codex's on the right.
    let left_title = region_text(&buf, 32, 55, 0);
    assert!(
        left_title.trim_start().starts_with("● claude-code"),
        "left title: {left_title}"
    );
    let right_title = region_text(&buf, 56, 80, 0);
    assert!(
        right_title.trim_start().starts_with("● codex"),
        "right title: {right_title}"
    );

    // The focused pane's title row is reversed; the unfocused one is not.
    assert!(buf
        .cell((56, 0))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
    assert!(!buf
        .cell((32, 0))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));

    // Content starts on row 1, under the titles.
    assert_eq!(region_text(&buf, 32, 55, 1), "left agent output");
    assert_eq!(region_text(&buf, 56, 80, 1), "right agent output");

    // Separator between the halves spans the pane rows.
    assert_eq!(buf.cell((55, 0)).unwrap().symbol(), "│");
    assert_eq!(buf.cell((55, 5)).unwrap().symbol(), "│");

    // Cursor lands one row lower than before, inside the focused content.
    terminal.backend_mut().assert_cursor_position((57u16, 1u16));

    // Sidebar + status still render.
    assert!(region_text(&buf, 0, 32, 0).starts_with(" roster"));
    assert!(region_text(&buf, 0, 80, 11).contains("ctrl-b"));
}

#[test]
fn launcher_modal_overlays_the_frame() {
    let now = Instant::now();
    let (session, left, right) = two_agent_session(now);
    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left agent output"));
    grids.insert(right, Grid::from_text("right agent output"));

    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let items = launch_items(&detector, "/bin/zsh");
    let state = LauncherState::new();
    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &entries,
        selected: None,
        side: SidebarSide::Left,
        launcher: Some((&items, &state)),
        mode_badge: Some("LAUNCH"),
        status: "type to filter",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    let all: String = (0..20)
        .map(|y| region_text(&buf, 0, 80, y) + "\n")
        .collect();
    assert!(all.contains("new agent"), "missing modal title:\n{all}");
    assert!(all.contains("claude-code"), "missing item:\n{all}");
    assert!(all.contains("shell"), "missing shell item:\n{all}");
}

#[test]
fn exited_pane_notice_and_title_marker() {
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
        launcher: None,
        mode_badge: Some("PREFIX"),
        status: "hints",
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Title marks the exit; notice sits on the pane's bottom content row.
    let title = region_text(&buf, 32, 80, 0);
    assert!(title.contains("exited"), "title: {title}");
    let notice = region_text(&buf, 32, 80, 8);
    assert!(
        notice.starts_with(" exited (3) — ctrl-b x to close"),
        "notice: {notice}"
    );

    let status = region_text(&buf, 0, 80, 9);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
}
