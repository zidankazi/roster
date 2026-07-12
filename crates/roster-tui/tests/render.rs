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
    launch_items, muted, render, selected, selected_muted, sidebar_entries, state_color, Hit,
    LauncherState, SidebarSide, View, ACCENT,
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

    // The chrome stands back from the terminal edge: rows 0 and 11 and
    // columns 0-1 are bare canvas — empty text, painted in the base
    // surface (the fill IS the region separation, so bg is the point).
    assert_eq!(region_text(&buf, 0, 80, 0), "");
    assert_eq!(region_text(&buf, 0, 80, 11), "");
    for (x, y) in [(0, 0), (40, 0), (0, 6), (79, 11)] {
        assert_eq!(
            buf.cell((x, y)).unwrap().style().bg,
            Some(roster_tui::SURFACE_BASE),
            "margin cell ({x},{y})"
        );
    }
    // The gap column between sidebar and panes sits on the same canvas.
    assert_eq!(
        buf.cell((33, 5)).unwrap().style().bg,
        Some(roster_tui::SURFACE_BASE)
    );

    // Each pane sits in a rounded panel (34..56 and 56..78, rows 1..10);
    // the title rides the top border: the working claude pane's on the
    // left (spinner frame at tick 0), the blocked one's on the right.
    let left_title = region_text(&buf, 34, 56, 1);
    assert!(
        left_title.contains("⠋ claude-code"),
        "left title: {left_title}"
    );
    let right_title = region_text(&buf, 56, 78, 1);
    assert!(
        right_title.contains("◉ claude-code"),
        "right title: {right_title}"
    );
    assert_eq!(buf.cell((34, 1)).unwrap().symbol(), "╭");
    assert_eq!(buf.cell((34, 9)).unwrap().symbol(), "╰");

    // Focus reads as the accent border and an accent-colored name — not a
    // heavy inverse bar. The unfocused panel's border stays muted.
    assert_eq!(buf.cell((56, 1)).unwrap().symbol(), "╭");
    assert_eq!(buf.cell((56, 1)).unwrap().style().fg, Some(ACCENT));
    assert_eq!(buf.cell((60, 1)).unwrap().style().fg, Some(ACCENT));
    assert_eq!(buf.cell((34, 1)).unwrap().style().fg, muted().fg);
    assert!(!buf
        .cell((56, 1))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));

    // Content starts inside the panels, under the border row.
    assert_eq!(region_text(&buf, 35, 55, 2), "left agent output");
    assert_eq!(region_text(&buf, 57, 77, 2), "right agent output");

    // The adjacent panel borders separate the halves — no bare rule.
    assert_eq!(buf.cell((55, 5)).unwrap().symbol(), "│");
    assert_eq!(buf.cell((56, 5)).unwrap().symbol(), "│");

    // Cursor lands inside the focused panel's content.
    terminal.backend_mut().assert_cursor_position((58u16, 2u16));

    // Sidebar header renders inside the inset; the old rule column is now
    // a bare gap — spacing separates the regions.
    assert!(region_text(&buf, 2, 33, 1).trim().starts_with("agents"));
    assert_eq!(buf.cell((33, 1)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((33, 5)).unwrap().symbol(), " ");
    assert!(region_text(&buf, 0, 80, 10).contains("ctrl-b"));

    // Mouse-first chrome: the pinned + new agent pill on the sidebar's
    // bottom row (a reversed pill, not bracketed text), a ✕ close button
    // punched into each title border, and the grid · solo switcher pills
    // on the status row (two panes exist).
    assert_eq!(region_text(&buf, 2, 33, 9).trim(), "+ new agent");
    assert!(buf
        .cell((3, 9))
        .unwrap()
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
    // The row above the button is breathing room now — the switcher left
    // the sidebar.
    assert_eq!(region_text(&buf, 2, 33, 8), "");
    assert_eq!(buf.cell((53, 1)).unwrap().symbol(), "✕");
    assert_eq!(buf.cell((75, 1)).unwrap().symbol(), "✕");
    // On the status row (10): grid at 64..70, solo at 71..77. The active
    // layout's pill is accent-filled; the inactive one is a muted pill —
    // an explicit, legible foreground rather than the near-invisible DIM
    // attribute an earlier fix replaced.
    assert_eq!(region_text(&buf, 64, 78, 10).trim(), "grid   solo");
    let grid = buf.cell((65, 10)).unwrap().style();
    assert_eq!(grid.fg, Some(ACCENT));
    assert!(grid.add_modifier.contains(Modifier::REVERSED));
    let solo = buf.cell((72, 10)).unwrap().style();
    assert_eq!(solo.fg, muted().fg);
    assert!(solo.add_modifier.contains(Modifier::REVERSED));
    assert!(!solo.add_modifier.contains(Modifier::DIM));
}

#[test]
fn pane_title_prefers_the_panes_terminal_title_over_the_agent_name() {
    // The border has the width the sidebar card lacks: a pane whose agent
    // broadcast a task title shows it in full on the top border, and a
    // pane without one keeps the agent-name fallback.
    let now = Instant::now();
    let (mut session, left, right) = two_agent_session(now);
    session.set_title(left, Some("fix the launch button".into()));

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
        status: "",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Left pane (titled): the full task on the border, not `claude-code`.
    let left_title = region_text(&buf, 34, 66, 1);
    assert!(
        left_title.contains("fix the launch button"),
        "left title: {left_title}"
    );
    assert!(
        !left_title.contains("claude-code"),
        "left title: {left_title}"
    );
    // Right pane (untitled): the agent-name fallback stays.
    let right_title = region_text(&buf, 66, 98, 1);
    assert!(
        right_title.contains("claude-code"),
        "right title: {right_title}"
    );
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

    // Every listed element resolves to an explicit quiet foreground,
    // never `DIM`. The focused card is the inverted (light) surface, so
    // its quiet text carries the selected surface's dark tiers — the same
    // guarantee in the flipped palette.
    let assert_quiet = |x: u16, y: u16, expected: Option<ratatui::style::Color>, what: &str| {
        let style = buf.cell((x, y)).unwrap().style();
        assert_eq!(style.fg, expected, "{what} at ({x},{y}) is off-tier");
        assert!(
            !style.add_modifier.contains(Modifier::DIM),
            "{what} at ({x},{y}) still leans on DIM"
        );
    };
    // Sidebar "agents" subtitle (first glyph after the leading space,
    // inside the chrome inset).
    assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "a");
    assert_quiet(3, 1, muted().fg, "sidebar subtitle");
    // The blocked card leads (12s old) and holds focus — the inverted
    // card: the focus bar on its edge, its right-aligned age column…
    assert_eq!(
        region_text(&buf, 2, 33, 3),
        "▍ ◉ claude-code            12s"
    );
    assert_quiet(29, 3, selected_muted().fg, "sidebar age");
    // …and the reason leading the detail row (behind the focus bar's edge
    // column) — the glyph carries the state, the row carries the why, in
    // the surface's primary dark text.
    assert!(region_text(&buf, 2, 33, 4).starts_with("▍   Approve this"));
    assert_quiet(16, 4, selected().fg, "sidebar state reason");
    // The bottom status hint centers in the footer (41 drawn cells on a
    // 76-wide chrome row → column 19).
    assert_eq!(buf.cell((19, 10)).unwrap().symbol(), "c");
    assert_quiet(19, 10, muted().fg, "status hint");
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

    // Hovering a ✕ turns it the danger red (the blocked state's fixed
    // hue) and bold; unhovered it stays muted. The buttons ride the title
    // borders (row 1) at 53 and 75.
    let buf = draw(Some(Hit::PaneClose(left)));
    let close = buf.cell((53, 1)).unwrap();
    assert_eq!(close.symbol(), "✕");
    assert_eq!(close.style().fg, Some(state_color(AgentState::Blocked)));
    assert!(close.style().add_modifier.contains(Modifier::BOLD));
    let other = buf.cell((75, 1)).unwrap();
    assert_eq!(other.style().fg, muted().fg);
    assert!(!other.style().add_modifier.contains(Modifier::DIM));

    // The + new agent pill is reversed at rest — that's its button shape —
    // and hover underlines it.
    let rested = draw(None);
    let rest = rested.cell((3, 9)).unwrap().style();
    assert!(rest.add_modifier.contains(Modifier::REVERSED));
    assert!(!rest.add_modifier.contains(Modifier::UNDERLINED));
    let buf = draw(Some(Hit::SidebarNewAgent));
    let hovered = buf.cell((3, 9)).unwrap().style();
    assert!(hovered.add_modifier.contains(Modifier::REVERSED));
    assert!(hovered.add_modifier.contains(Modifier::UNDERLINED));

    // Hovering a sidebar card shows a quiet muted marker.
    let buf = draw(Some(Hit::SidebarEntry(1)));
    let marker = buf.cell((2, 6)).unwrap();
    assert_eq!(marker.symbol(), "❯");
    assert_eq!(marker.style().fg, muted().fg);

    // On the focused — inverted — card the marker keeps the quiet tier in
    // the flipped palette, so it never washes out on the light fill.
    let buf = draw(Some(Hit::SidebarEntry(0)));
    let marker = buf.cell((2, 3)).unwrap();
    assert_eq!(marker.symbol(), "❯");
    assert_eq!(marker.style().fg, selected_muted().fg);

    // No hover: the ✕ sits muted (not lit red), and no card marker shows.
    let buf = draw(None);
    let close = buf.cell((53, 1)).unwrap().style();
    assert_eq!(close.fg, muted().fg);
    assert_ne!(close.fg, Some(state_color(AgentState::Blocked)));
    assert_ne!(buf.cell((2, 3)).unwrap().symbol(), "❯");
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

    // One full-width panel — the focused pane's — and no interior border
    // between halves (column 55 is inside the solo panel).
    let title = region_text(&buf, 34, 78, 1);
    assert!(title.contains("◉ claude-code"), "title: {title}");
    assert_ne!(buf.cell((55, 5)).unwrap().symbol(), "│");

    // The solo pane's content spans the panel interior; the hidden pane's
    // content is nowhere.
    assert_eq!(region_text(&buf, 35, 77, 2), "right agent output");
    let all: String = (0..12)
        .map(|y| region_text(&buf, 0, 80, y) + "\n")
        .collect();
    assert!(!all.contains("left agent output"), "screen:\n{all}");

    // Sidebar still lists every agent — it is the switcher — and the
    // status row's layout pills show solo active (accent-filled).
    assert!(all.contains("claude-code"), "screen:\n{all}");
    assert_eq!(region_text(&buf, 64, 78, 10).trim(), "grid   solo");
    let solo = buf.cell((72, 10)).unwrap().style();
    assert_eq!(solo.fg, Some(ACCENT));
    assert!(solo.add_modifier.contains(Modifier::REVERSED));
    let grid = buf.cell((65, 10)).unwrap().style();
    assert_eq!(grid.fg, muted().fg);
}

#[test]
fn degenerate_frames_render_without_panicking() {
    let now = Instant::now();
    let (mut session, left, right) = two_agent_session(now);
    session.focus(right);
    let mut grids = HashMap::new();
    grids.insert(left, Grid::from_text("left"));
    grids.insert(right, Grid::from_text("right"));
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let scrolled = HashMap::new();
    let items = launch_items(&detector);
    let state = LauncherState::new();
    // Sizes straddling every threshold: the inset gate (24x8 and just
    // under), non-panelled slivers, near-zero frames, and the
    // drawable-but-cramped band (5-15 rows) where a modal passes its size
    // floor but cannot hold its full layout. Each size is drawn with
    // every overlay — no overlay, the mid-session launcher, the welcome
    // launcher, and the confirm dialog.
    for (w, h) in [
        (1, 1),
        (5, 2),
        (10, 3),
        (23, 7),
        (23, 8),
        (24, 7),
        (24, 8),
        (25, 9),
        (30, 4),
        (80, 3),
        (80, 5),
        (80, 6),
        (80, 10),
        (80, 15),
    ] {
        for overlay in 0..4 {
            let launcher = (overlay == 1 || overlay == 2).then_some((items.as_slice(), &state));
            let confirm = (overlay == 3).then_some(None);
            let view = View {
                session: &session,
                grids: &grids,
                exited: &exited,
                entries: &entries,
                selected: None,
                hover: None,
                zoomed: false,
                side: SidebarSide::Left,
                launcher,
                confirm,
                toasts: &[],
                selection: None,
                scrolled: &scrolled,
                welcome: overlay == 2,
                mode_badge: Some("PREFIX"),
                status: "c: new agent · q: quit",
                tick: 0,
            };
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| render(frame, &view))
                .unwrap_or_else(|_| panic!("render panicked at {w}x{h} overlay {overlay}"));
        }
    }
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
        mode_badge: None,
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
        mode_badge: None,
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

    // The title border marks the exit; the pane hosts the overlay card
    // with its restart/close buttons.
    let title = region_text(&buf, 34, 78, 1);
    assert!(title.contains("exited"), "title: {title}");
    let all: String = (0..10u16)
        .map(|y| region_text(&buf, 34, 78, y) + "\n")
        .collect();
    assert!(all.contains("claude · exit 3"), "card message:\n{all}");
    assert!(all.contains("restart"), "restart button:\n{all}");
    assert!(all.contains("close"), "close button:\n{all}");

    let status = region_text(&buf, 2, 80, 8);
    assert!(status.starts_with(" PREFIX "), "status: {status}");
}

#[test]
fn exited_marker_survives_a_long_task_title() {
    // The task title yields cells to the ` · exited` marker: a truncated
    // name still reads, a truncated marker vanishes.
    let now = Instant::now();
    let mut session = Session::new();
    let only = session.focused().unwrap();
    session.pane_mut(only).unwrap().command = Some("claude".into());
    session.set_reading(only, AgentState::Idle, None, now);
    session.set_title(
        only,
        Some("a very long task title that keeps going far past the border budget".into()),
    );

    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text("final output"));
    let mut exited = HashMap::new();
    exited.insert(only, 3u32);
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);

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
        status: "",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    let title = region_text(&buf, 34, 78, 1);
    assert!(
        title.contains("a very long task"),
        "title lost the task: {title}"
    );
    assert!(
        title.contains("· exited"),
        "title lost the exit marker: {title}"
    );
}

#[test]
fn exited_marker_survives_a_wide_char_task_title() {
    // The same guarantee measured in cells: agent task titles carry wide
    // chars verbatim, and a run of double-width glyphs must not cost the
    // ` · exited` marker its tail.
    let now = Instant::now();
    let mut session = Session::new();
    let only = session.focused().unwrap();
    session.pane_mut(only).unwrap().command = Some("claude".into());
    session.set_reading(only, AgentState::Idle, None, now);
    session.set_title(
        only,
        Some("修复认证模块的错误处理逻辑然后重新运行测试修复认证模块的错误处理".into()),
    );

    let mut grids = HashMap::new();
    grids.insert(only, Grid::from_text("final output"));
    let mut exited = HashMap::new();
    exited.insert(only, 3u32);
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);

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
        status: "",
        tick: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| render(frame, &view)).unwrap();
    let buf = terminal.backend().buffer().clone();

    let title = region_text(&buf, 34, 78, 1);
    // Wide glyphs read back with placeholder-cell gaps, so match one char.
    assert!(title.contains('修'), "title lost the task: {title}");
    assert!(
        title.contains("· exited"),
        "title lost the exit marker: {title}"
    );
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

    // 50 wide → chrome 46, sidebar 23, panel interior 21 — too narrow for
    // the 30-col card, so the one-line strip stays.
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
        .map(|y| region_text(&buf, 2, 33, y) + "\n")
        .collect();
    assert!(
        !sidebar.contains("workspace"),
        "flat view must drop workspace headers:\n{sidebar}"
    );

    // Each card carries a `⧉N` workspace tag: the top card is the blocked
    // agent from window 1 (⧉2), the next is the idle one from window 0 (⧉1).
    let card0 = region_text(&buf, 2, 33, 3);
    assert!(
        card0.contains("⧉2"),
        "top card should tag window 2: {card0}"
    );
    let card1 = region_text(&buf, 2, 33, 6);
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

#[test]
fn done_pulse_keeps_sidebar_and_title_glyphs_in_step() {
    // The pulse is a property of the glyph, not of the sidebar: the same
    // done pane must flip to reversed — and back — in its sidebar card and
    // its pane title bar on the same tick.
    let now = Instant::now();
    let mut session = Session::new();
    let pane = session.focused().unwrap();
    session.pane_mut(pane).unwrap().command = Some("claude".into());
    session.set_reading(pane, AgentState::Done, Some("finished".into()), now);
    let mut grids = HashMap::new();
    grids.insert(pane, Grid::from_text("agent output"));
    let detector = Detector::builtin();
    let entries = sidebar_entries(&session, &detector, now);
    let exited = HashMap::new();
    let scrolled = HashMap::new();

    let glyph_styles = |tick: u64| {
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
            status: "",
            tick,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let buf = terminal.backend().buffer().clone();
        // The card glyph sits two columns into the sidebar's first card
        // row (inside the chrome inset); the title glyph two columns into
        // the panel's top border.
        assert_eq!(buf.cell((4, 3)).unwrap().symbol(), "✓");
        assert_eq!(buf.cell((36, 1)).unwrap().symbol(), "✓");
        (
            buf.cell((4, 3)).unwrap().style(),
            buf.cell((36, 1)).unwrap().style(),
        )
    };
    let (card_off, title_off) = glyph_styles(0);
    let (card_on, title_on) = glyph_styles(4);
    assert!(!card_off.add_modifier.contains(Modifier::REVERSED));
    assert!(card_on.add_modifier.contains(Modifier::REVERSED));
    // The palettes differ on purpose — the focused card is the inverted
    // surface, the title sits on the dark chrome — but the pulse *phase*
    // must flip in step in both places, and neither glyph may ever pass
    // through a foregroundless frame.
    assert_eq!(
        card_off.add_modifier, title_off.add_modifier,
        "steady phase diverged"
    );
    assert_eq!(
        card_on.add_modifier, title_on.add_modifier,
        "reversed phase diverged"
    );
    for (what, style) in [
        ("card off", card_off),
        ("card on", card_on),
        ("title off", title_off),
        ("title on", title_on),
    ] {
        assert!(style.fg.is_some(), "{what} phase dropped its foreground");
    }
    assert_eq!(card_off.fg, card_on.fg, "the card pulse changes color");
    assert_eq!(title_off.fg, title_on.fg, "the title pulse changes color");
}
