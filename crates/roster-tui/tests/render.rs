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
    launch_items, muted, render, sidebar_entries, Hit, LauncherState, SidebarSide, View, ACCENT,
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
    session.pane_mut(right).unwrap().command = Some("claude".into());
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
    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: false,
        mode_badge: None,
        status: "hints with ctrl-b",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Row 0 of the pane region holds title bars, not content: the working
    // claude pane's title on the left half (spinner frame at tick 0), the
    // blocked claude pane's on the right.
    let left_title = region_text(&buf, 32, 55, 0);
    assert!(
        left_title.trim_start().starts_with("⠋ claude-code"),
        "left title: {left_title}"
    );
    let right_title = region_text(&buf, 56, 80, 0);
    assert!(
        right_title.contains("◉ claude-code"),
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
    // Grid is the active layout (accent); the inactive solo word is muted —
    // an explicit, legible foreground rather than the near-invisible DIM
    // attribute this fix replaced.
    assert_eq!(buf.cell((1, 9)).unwrap().style().fg, Some(ACCENT));
    let solo = buf.cell((8, 9)).unwrap().style();
    assert_eq!(solo.fg, muted().fg);
    assert!(!solo.add_modifier.contains(Modifier::DIM));
}

#[test]
fn secondary_chrome_is_muted_not_the_faint_dim_attribute() {
    // Regression guard for the low-contrast chrome bug: on a terminal's
    // default palette, `Modifier::DIM` renders as a near-invisible faint
    // gray. The elements the bug report called out — the sidebar "agents"
    // subtitle, an agent's age and its state reason, and the bottom status
    // hint — must now carry an explicit muted foreground and no `DIM`.
    let now = Instant::now();
    let (session, left, right) = two_agent_session(now);
    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left agent output"));
    grids.insert(right, Grid::from_text("right agent output"));
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: false,
        mode_badge: None,
        status: "click a pane to focus · ctrl-b for keys",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Every listed element resolves to the muted foreground, never `DIM`.
    let muted_fg = muted().fg;
    let assert_muted = |x: u16, y: u16, what: &str| {
        let style = buf.cell((x, y)).unwrap().style();
        assert_eq!(style.fg, muted_fg, "{what} at ({x},{y}) is not muted");
        assert!(
            !style.add_modifier.contains(Modifier::DIM),
            "{what} at ({x},{y}) still leans on DIM"
        );
    };
    // Sidebar "agents" subtitle (first glyph after the leading space).
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "a");
    assert_muted(1, 0, "sidebar subtitle");
    // The blocked card leads (12s old): its right-aligned age column…
    assert_eq!(
        region_text(&buf, 0, 31, 2),
        "  ◉ claude-code            12s"
    );
    assert_muted(27, 2, "sidebar age");
    // …and the state reason after the colored state word on the detail row.
    assert!(region_text(&buf, 0, 31, 3).starts_with("    blocked · Approve"));
    assert_muted(14, 3, "sidebar state reason");
    // The bottom status hint line spans from the left edge.
    assert_eq!(buf.cell((0, 11)).unwrap().symbol(), "c");
    assert_muted(0, 11, "status hint");
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
        let scrolled = HashMap::new();
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
            confirm: None,
            toasts: &[],
            selection: None,
            scrolled: &scrolled,
            welcome: false,
            mode_badge: None,
            status: "",
            tick: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        terminal.backend().buffer().clone()
    };

    // Hovering a ✕ turns it red and bold; unhovered it stays muted.
    let buf = draw(Some(Hit::PaneClose(left)));
    let close = buf.cell((53, 0)).unwrap();
    assert_eq!(close.symbol(), "✕");
    assert_eq!(close.style().fg, Some(ratatui::style::Color::Red));
    assert!(close.style().add_modifier.contains(Modifier::BOLD));
    let other = buf.cell((78, 0)).unwrap();
    assert_eq!(other.style().fg, muted().fg);
    assert!(!other.style().add_modifier.contains(Modifier::DIM));

    // Hovering the + new agent button inverts it.
    let buf = draw(Some(Hit::SidebarNewAgent));
    assert!(buf
        .cell((1, 10))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));

    // Hovering a sidebar card shows a quiet muted marker.
    let buf = draw(Some(Hit::SidebarEntry(0)));
    let marker = buf.cell((0, 2)).unwrap();
    assert_eq!(marker.symbol(), "❯");
    assert_eq!(marker.style().fg, muted().fg);

    // No hover: the ✕ sits muted (not lit red), and no card marker shows.
    let buf = draw(None);
    let close = buf.cell((53, 0)).unwrap().style();
    assert_eq!(close.fg, muted().fg);
    assert_ne!(close.fg, Some(ratatui::style::Color::Red));
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
    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
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
    assert!(title.contains("◉ claude-code"), "title: {title}");
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
    let items = launch_items(&detector);
    let state = LauncherState::new();
    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
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
}

#[test]
fn welcome_screen_shows_wordmark_picker_and_command_hint() {
    let session = Session::new();
    let only = session.focused().unwrap();
    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text("zidan@mac ~ %"));

    let detector = Detector::builtin();
    let exited = HashMap::new();
    let items = launch_items(&detector);
    let state = LauncherState::new();
    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: true,
        mode_badge: Some("LAUNCH"),
        status: "type to filter",
        // Past the reveal: the wordmark is fully on screen.
        tick: 99,
    };

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let all: String = (0..24)
        .map(|y| region_text(&buf, 0, 100, y) + "\n")
        .collect();

    // The solid-fill wordmark, tagline, picker, and the run-a-command hint
    // all render.
    assert!(all.contains("7Mb,od8"), "screen:\n{all}");
    assert!(
        all.contains("terminal multiplexer for Claude Code"),
        "screen:\n{all}"
    );
    assert!(all.contains("claude-code"), "screen:\n{all}");
    assert!(all.contains("run a command"), "screen:\n{all}");
    // Nothing else on screen: no modal chrome, no sidebar, no status
    // line, and the placeholder shell's prompt is hidden.
    assert!(!all.contains("new agent ─"), "screen:\n{all}");
    assert!(!all.contains("+ new agent"), "screen:\n{all}");
    assert!(!all.contains("type to filter"), "screen:\n{all}");
    assert!(!all.contains("zidan@mac"), "screen:\n{all}");
}

#[test]
fn welcome_wordmark_reveals_with_the_tick() {
    let session = Session::new();
    let only = session.focused().unwrap();
    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text(""));
    let detector = Detector::builtin();
    let exited = HashMap::new();
    let items = launch_items(&detector);
    let state = LauncherState::new();

    let draw = |tick: u64| -> String {
        let scrolled = HashMap::new();
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
            confirm: None,
            toasts: &[],
            selection: None,
            scrolled: &scrolled,
            welcome: true,
            mode_badge: None,
            status: "",
            tick,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..24)
            .map(|y| region_text(&buf, 0, 100, y) + "\n")
            .collect()
    };

    // At tick 0 none of the wordmark is on screen; by tick 2 its leading
    // columns are; late ticks show the whole mark. (Checked across a few
    // beats — the ambient flicker may perturb any single frame.)
    assert!(!draw(0).contains("7Mb"), "tick 0 leaked wordmark");
    assert!(draw(2).contains("7Mb"), "tick 2 missing leading columns");
    assert!(
        (48..64).any(|t| draw(t).contains("`Mbmo`Mbmmd'")),
        "wordmark never fully revealed"
    );
    // The picker is usable from the first frame regardless.
    assert!(draw(0).contains("claude-code"), "picker not immediate");

    // The ambient flicker: stand-in glyphs (never part of the wordmark)
    // blink in and out across beats, only inside the revealed mark.
    let flickered = |s: &str| s.chars().any(|c| "*+~#".contains(c));
    assert!(
        (20..44).any(|t| flickered(&draw(t))),
        "no flicker across two dozen ticks"
    );
    assert!(!flickered(&draw(0)), "flicker outside the revealed mark");
    // Beats change what flickers: consecutive beats differ somewhere.
    assert!(
        (48..64).any(|t| draw(t) != draw(t + 2)),
        "animation is static"
    );
}

#[test]
fn exited_pane_overlay_card_and_title_marker() {
    let mut session = Session::new();
    let only = session.focused().unwrap();
    session.pane_mut(only).unwrap().command = Some("claude".into());
    let mut grid = Grid::from_text("final output");
    grid.cursor.visible = true;

    let mut grids = HashMap::new();
    grids.insert(only, grid);
    let mut exited = HashMap::new();
    exited.insert(only, 3u32);

    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: false,
        mode_badge: Some("PREFIX"),
        status: "hints",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Title marks the exit; the pane hosts the overlay card with its
    // restart/close buttons.
    let title = region_text(&buf, 32, 80, 0);
    assert!(title.contains("exited"), "title: {title}");
    let all: String = (0..10u16)
        .map(|y| region_text(&buf, 32, 80, y) + "\n")
        .collect();
    assert!(all.contains("claude · exit 3"), "card message:\n{all}");
    assert!(all.contains("restart"), "restart button:\n{all}");
    assert!(all.contains("close"), "close button:\n{all}");

    let status = region_text(&buf, 0, 80, 9);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
}

#[test]
fn exited_pane_too_small_for_the_card_keeps_the_strip() {
    let mut session = Session::new();
    let only = session.focused().unwrap();
    session.pane_mut(only).unwrap().command = Some("claude".into());
    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text("out"));
    let mut exited = HashMap::new();
    exited.insert(only, 3u32);

    let scrolled = HashMap::new();
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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: false,
        mode_badge: None,
        status: "hints",
        tick: 0,
    };

    // 50 wide → sidebar 25, pane content 25 — too narrow for the 30-col
    // card, so the one-line strip stays.
    let mut terminal = Terminal::new(TestBackend::new(50, 8)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let all: String = (0..8u16)
        .map(|y| region_text(&buf, 25, 50, y) + "\n")
        .collect();
    assert!(all.contains("exited (3)"), "strip fallback missing:\n{all}");
    assert!(!all.contains("restart"), "card should not fit:\n{all}");
}

fn two_workspace_session(now: Instant) -> Session {
    // Window 0: an idle agent. Window 1: a blocked one. One pane each, so
    // the grid·solo switcher stays hidden.
    let mut session = Session::new();
    let a = session.focused().unwrap();
    session.pane_mut(a).unwrap().command = Some("claude".into());
    session.set_reading(a, AgentState::Idle, None, now - Duration::from_secs(5));
    let b = session.new_window();
    session.pane_mut(b).unwrap().command = Some("claude".into());
    session.set_reading(
        b,
        AgentState::Blocked,
        Some("Approve?".into()),
        now - Duration::from_secs(9),
    );
    session
}

#[test]
fn sidebar_ranks_globally_across_workspaces_and_tags_cards() {
    let now = Instant::now();
    let session = two_workspace_session(now);
    let grids = HashMap::new();
    let exited = HashMap::new();
    let scrolled = HashMap::new();
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    // Global ranking: the blocked agent in window 1 leads the idle one in
    // window 0, and the workspace no longer groups them.
    assert_eq!(entries[0].window, 1);
    assert_eq!(entries[1].window, 0);

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
        confirm: None,
        toasts: &[],
        selection: None,
        scrolled: &scrolled,
        welcome: false,
        mode_badge: None,
        status: "hints",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // No workspace headers in the flat view — the whole sidebar is cards.
    let sidebar: String = (0..20)
        .map(|y| region_text(&buf, 0, 31, y) + "\n")
        .collect();
    assert!(
        !sidebar.contains("workspace"),
        "flat view must drop workspace headers:\n{sidebar}"
    );

    // Each card carries a `⧉N` workspace tag: the top card is the blocked
    // agent from window 1 (⧉2), the next is the idle one from window 0 (⧉1).
    let card0 = region_text(&buf, 0, 31, 2);
    assert!(
        card0.contains("⧉2"),
        "top card should tag window 2: {card0}"
    );
    let card1 = region_text(&buf, 0, 31, 5);
    assert!(
        card1.contains("⧉1"),
        "next card should tag window 1: {card1}"
    );

    // The removed `by space · by need` switcher must leave no residue: the
    // ranking has exactly one scope now, so the sidebar hosts no such row.
    assert!(
        !sidebar.contains("by space") && !sidebar.contains("by need"),
        "no triage switcher may render:\n{sidebar}"
    );
}
