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
use roster_tui::{
    launch_items, render, sidebar_entries, Hit, LauncherState, SidebarSide, View, ACCENT,
};

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
        hover: None,
        zoomed: false,
        side: SidebarSide::Left,
        launcher: None,
        welcome: false,
        mode_badge: None,
        status: "hints with ctrl-b",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Row 0 of the pane region holds title bars, not content: the working
    // claude pane's title on the left half (spinner frame at tick 0),
    // blocked codex's on the right.
    let left_title = region_text(&buf, 32, 55, 0);
    assert!(
        left_title.trim_start().starts_with("⠋ claude-code"),
        "left title: {left_title}"
    );
    let right_title = region_text(&buf, 56, 80, 0);
    assert!(
        right_title.contains("◉ codex"),
        "right title: {right_title}"
    );

    // Focus reads as an accent marker and accent-colored name — not a
    // heavy inverse bar.
    assert_eq!(buf.cell((56, 0)).unwrap().symbol(), "▎");
    assert_eq!(buf.cell((60, 0)).unwrap().style().fg, Some(ACCENT));
    assert_ne!(buf.cell((32, 0)).unwrap().symbol(), "▎");
    assert!(!buf
        .cell((56, 0))
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

    // Sidebar header, the sidebar/pane rule, and the status line render.
    assert!(region_text(&buf, 0, 31, 0).trim().starts_with("agents"));
    assert_eq!(buf.cell((31, 0)).unwrap().symbol(), "│");
    assert_eq!(buf.cell((31, 9)).unwrap().symbol(), "│");
    assert!(region_text(&buf, 0, 80, 11).contains("ctrl-b"));

    // Mouse-first chrome: the pinned + new agent button on the sidebar's
    // bottom row, the grid · solo switcher above it (two panes exist), and
    // a ✕ close button at each title's right edge.
    assert_eq!(region_text(&buf, 0, 31, 10).trim(), "+ new agent");
    assert_eq!(region_text(&buf, 0, 31, 9).trim(), "grid · solo");
    assert_eq!(buf.cell((53, 0)).unwrap().symbol(), "✕");
    assert_eq!(buf.cell((78, 0)).unwrap().symbol(), "✕");
    // Grid is the active layout: accent; solo is dim.
    assert_eq!(buf.cell((1, 9)).unwrap().style().fg, Some(ACCENT));
    assert!(buf
        .cell((8, 9))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::DIM));
}

#[test]
fn hover_lights_up_interactive_chrome() {
    let now = Instant::now();
    let (session, left, right) = two_agent_session(now);
    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left"));
    grids.insert(right, Grid::from_text("right"));
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();

    let draw = |hover: Option<Hit>| -> Buffer {
        let view = View {
            session: &session,
            grids: &grids,
            exited: &exited,
            entries: &entries,
            selected: None,
            hover,
            zoomed: false,
            side: SidebarSide::Left,
            launcher: None,
            welcome: false,
            mode_badge: None,
            status: "",
            tick: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        terminal.backend().buffer().clone()
    };

    // Hovering a ✕ turns it red and bold; unhovered it stays dim.
    let buf = draw(Some(Hit::PaneClose(left)));
    let close = buf.cell((53, 0)).unwrap();
    assert_eq!(close.symbol(), "✕");
    assert_eq!(close.style().fg, Some(ratatui::style::Color::Red));
    assert!(close.style().add_modifier.contains(Modifier::BOLD));
    let other = buf.cell((78, 0)).unwrap();
    assert!(other.style().add_modifier.contains(Modifier::DIM));

    // Hovering the + new agent button inverts it.
    let buf = draw(Some(Hit::SidebarNewAgent));
    assert!(buf
        .cell((1, 10))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));

    // Hovering a sidebar card shows a quiet marker.
    let buf = draw(Some(Hit::SidebarEntry(0)));
    let marker = buf.cell((0, 2)).unwrap();
    assert_eq!(marker.symbol(), "❯");
    assert!(marker.style().add_modifier.contains(Modifier::DIM));

    // No hover, no chrome lit.
    let buf = draw(None);
    assert!(buf
        .cell((53, 0))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::DIM));
    assert_ne!(buf.cell((0, 2)).unwrap().symbol(), "❯");
}

#[test]
fn solo_view_fills_the_pane_region_with_the_focused_pane() {
    let now = Instant::now();
    let (mut session, left, right) = two_agent_session(now);
    session.focus(right);
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
        hover: None,
        zoomed: true,
        side: SidebarSide::Left,
        launcher: None,
        welcome: false,
        mode_badge: Some("SOLO"),
        status: "click agents on the left to switch",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // One full-width title — the focused pane's — and no interior
    // separator column.
    let title = region_text(&buf, 32, 80, 0);
    assert!(title.contains("◉ codex"), "title: {title}");
    assert!(!title.contains("claude-code"), "title: {title}");
    assert_ne!(buf.cell((55, 5)).unwrap().symbol(), "│");

    // The solo pane's content spans the whole region; the hidden pane's
    // content is nowhere.
    assert_eq!(region_text(&buf, 32, 80, 1), "right agent output");
    let all: String = (0..12)
        .map(|y| region_text(&buf, 0, 80, y) + "\n")
        .collect();
    assert!(!all.contains("left agent output"), "screen:\n{all}");

    // Sidebar still lists every agent — it is the switcher — and the
    // layout control shows solo active.
    assert!(all.contains("claude-code"), "screen:\n{all}");
    assert!(all.contains("grid · solo"), "screen:\n{all}");
    assert_eq!(buf.cell((8, 9)).unwrap().style().fg, Some(ACCENT));
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
        hover: None,
        zoomed: false,
        side: SidebarSide::Left,
        launcher: Some((&items, &state)),
        welcome: false,
        mode_badge: Some("LAUNCH"),
        status: "type to filter",
        tick: 0,
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
fn welcome_screen_shows_wordmark_picker_and_any_command_hint() {
    let session = Session::new();
    let only = session.focused().unwrap();
    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text("zidan@mac ~ %"));

    let detector = Detector::builtin();
    let exited = HashMap::new();
    let items = launch_items(&detector, "/bin/zsh");
    let state = LauncherState::new();
    let view = View {
        session: &session,
        grids: &grids,
        exited: &exited,
        entries: &[],
        selected: None,
        hover: None,
        zoomed: false,
        side: SidebarSide::Left,
        launcher: Some((&items, &state)),
        welcome: true,
        mode_badge: Some("LAUNCH"),
        status: "type to filter",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let all: String = (0..24)
        .map(|y| region_text(&buf, 0, 100, y) + "\n")
        .collect();

    // The wordmark, tagline, picker, and the any-command hint all render.
    assert!(
        all.contains(r"| '__/ _ \/ __| __/ _ \ '__|"),
        "screen:\n{all}"
    );
    assert!(all.contains("run your coding agents"), "screen:\n{all}");
    assert!(all.contains("claude-code"), "screen:\n{all}");
    assert!(all.contains("type any command"), "screen:\n{all}");
    // No modal chrome, and the placeholder shell's prompt is hidden.
    assert!(!all.contains("new agent ─"), "screen:\n{all}");
    assert!(!all.contains("zidan@mac"), "screen:\n{all}");
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
        hover: None,
        zoomed: false,
        side: SidebarSide::Left,
        launcher: None,
        welcome: false,
        mode_badge: Some("PREFIX"),
        status: "hints",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Title marks the exit; notice sits on the pane's bottom content row.
    let title = region_text(&buf, 32, 80, 0);
    assert!(title.contains("exited"), "title: {title}");
    let notice = region_text(&buf, 32, 80, 8);
    assert!(
        notice.starts_with(" exited (3) — click ✕ to close"),
        "notice: {notice}"
    );

    let status = region_text(&buf, 0, 80, 9);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
}
