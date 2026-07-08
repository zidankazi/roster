//! The agent-state sidebar: agents grouped by workspace, each rendered as a
//! card — a colored status glyph, the agent name, and its age on top; the
//! state, its reason, and the clickable `auto` (auto-approve) chip below;
//! and, only when the statusline bridge feeds one, a third line of
//! telemetry badges (see `telemetry.rs`). Blocked and done float to the top
//! of each workspace so the agents that need you are always in view.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use roster_core::{AgentState, PaneId, Session, Telemetry};
use roster_detect::Detector;

use crate::style::{muted, state_color, state_glyph, state_label};
use crate::telemetry::telemetry_line;

/// One sidebar row: an agent pane and everything shown about it.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarEntry {
    /// The pane this row describes.
    pub pane: PaneId,
    /// The workspace (window) index the pane belongs to.
    pub window: usize,
    /// The agent's config name, e.g. `claude-code`.
    pub agent: String,
    /// The committed state.
    pub state: AgentState,
    /// Why — shown under the agent name.
    pub reason: Option<String>,
    /// Time since the state last changed, if it ever has.
    pub age: Option<Duration>,
    /// Whether roster auto-approves this pane's permission asks. Set by the
    /// binary (which owns the auto-approve set); the sidebar only renders it.
    pub auto_approve: bool,
    /// The pane's live statusline telemetry. `None` — a pane without the
    /// bridge, or a feed gone stale — renders exactly the two-line card
    /// from before the field existed.
    pub telemetry: Option<Telemetry>,
}

/// Build the sidebar rows from the session: every pane whose command
/// identifies as a configured agent, grouped by workspace and, within a
/// workspace, sorted so blocked and done float up and the longest-waiting
/// rows lead.
pub fn sidebar_entries(session: &Session, detector: &Detector, now: Instant) -> Vec<SidebarEntry> {
    let mut entries: Vec<SidebarEntry> = session
        .panes()
        .into_iter()
        .filter_map(|pane| {
            let command = pane.command.as_deref()?;
            let kind = detector.identify(command)?;
            Some(SidebarEntry {
                pane: pane.id,
                window: session.window_of(pane.id).unwrap_or(0),
                agent: detector.agent(kind).name.clone(),
                state: pane.state,
                reason: pane.reason.clone(),
                age: pane.last_change.map(|at| now.saturating_duration_since(at)),
                // The binary lights this from its auto-approve set; the
                // session model here has no notion of it.
                auto_approve: false,
                telemetry: pane.telemetry.clone(),
            })
        })
        .collect();
    entries.sort_by_key(|e| {
        (
            e.window,
            state_rank(e.state),
            std::cmp::Reverse(e.age.unwrap_or(Duration::ZERO)),
            e.pane,
        )
    });
    entries
}

fn state_rank(state: AgentState) -> u8 {
    match state {
        AgentState::Blocked => 0,
        AgentState::Done => 1,
        AgentState::Working => 2,
        AgentState::Idle => 3,
    }
}

/// A pane-switch request surfaced by the sidebar. The binary owns the
/// side effect; this crate only ever emits the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    /// Focus the given pane.
    JumpToPane(PaneId),
}

/// Keyboard-navigation state for the sidebar: which row is selected.
#[derive(Clone, Copy, Debug, Default)]
pub struct SidebarState {
    selected: usize,
}

impl SidebarState {
    /// A state with the first row selected.
    pub fn new() -> Self {
        SidebarState::default()
    }

    /// The selected row index, clamped to `len`; `None` when there are no
    /// rows.
    pub fn selected(&self, len: usize) -> Option<usize> {
        (len > 0).then(|| self.selected.min(len - 1))
    }

    /// Move the selection down one row, wrapping.
    pub fn select_next(&mut self, len: usize) {
        if len > 0 {
            self.selected = (self.selected.min(len - 1) + 1) % len;
        }
    }

    /// Move the selection up one row, wrapping.
    pub fn select_prev(&mut self, len: usize) {
        if len > 0 {
            let current = self.selected.min(len - 1);
            self.selected = (current + len - 1) % len;
        }
    }

    /// The intent behind pressing enter: jump to the selected entry's pane.
    pub fn activate(&self, entries: &[SidebarEntry]) -> Option<Message> {
        let index = self.selected(entries.len())?;
        Some(Message::JumpToPane(entries[index].pane))
    }
}

/// One row of the sidebar's card region, in order from the top. Render
/// draws this plan and hit-testing mirrors it, so the two can't drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    /// A workspace header (window index). Clickable: jumps to the window.
    Header(usize),
    /// The first line of an entry's card: glyph, name, age.
    EntryName(usize),
    /// The second line of an entry's card: state and reason.
    EntryDetail(usize),
    /// The third line of an entry's card: telemetry badges. Emitted only
    /// when the entry carries telemetry, so bridge-less cards keep their
    /// exact two-line shape.
    EntryTelemetry(usize),
    /// An agent-less workspace's placeholder line (window index).
    Empty(usize),
    /// Breathing room.
    Blank,
}

/// The sidebar's card-region rows for `entries` across `workspaces`
/// windows. With a single window there are no headers — just the cards.
/// With several, every workspace gets a header even when no agent runs in
/// it, so plain-shell windows stay reachable by mouse.
pub fn sidebar_rows(entries: &[SidebarEntry], workspaces: usize) -> Vec<SidebarRow> {
    fn push_card(rows: &mut Vec<SidebarRow>, index: usize, entry: &SidebarEntry) {
        rows.push(SidebarRow::EntryName(index));
        rows.push(SidebarRow::EntryDetail(index));
        if entry.telemetry.is_some() {
            rows.push(SidebarRow::EntryTelemetry(index));
        }
        rows.push(SidebarRow::Blank);
    }
    let mut rows = Vec::new();
    if workspaces <= 1 {
        for (index, entry) in entries.iter().enumerate() {
            push_card(&mut rows, index, entry);
        }
        return rows;
    }
    for window in 0..workspaces {
        rows.push(SidebarRow::Header(window));
        let mut any = false;
        for (index, entry) in entries.iter().enumerate() {
            if entry.window == window {
                any = true;
                push_card(&mut rows, index, entry);
            }
        }
        if !any {
            rows.push(SidebarRow::Empty(window));
            rows.push(SidebarRow::Blank);
        }
    }
    rows
}

/// The `auto` chip's text — the per-card auto-approve toggle.
const AUTO_CHIP: &str = "auto";

/// The minimum reason budget the chip must leave on its row. The reason is
/// the signal and outranks chrome: a sidebar too cramped for both drops
/// the chip (the keyboard toggle still works), never the reason.
const MIN_REASON: u16 = 8;

/// The columns of an entry detail row's `auto` chip, in sidebar-inner
/// columns: the word right-aligned one column in from the edge, mirroring
/// the age on the name row above. `None` when the row can't host it and a
/// useful reason. Deliberately width-only: sized to the *longest* state
/// label so the chip — and its click target — never flickers as a card's
/// state changes value. Render draws it and `hit_test` targets it, so the
/// chip can't drift off its click target.
pub fn auto_chip_cols(width: u16) -> Option<std::ops::Range<u16>> {
    let chip = AUTO_CHIP.chars().count() as u16;
    let longest_label = [
        AgentState::Blocked,
        AgentState::Working,
        AgentState::Done,
        AgentState::Idle,
    ]
    .into_iter()
    .map(|state| state_label(state).chars().count())
    .max()
    .unwrap_or(0) as u16;
    // Card indent + the longest state word + a gap + the reason's reserve
    // + the gutter before the chip.
    let taken = 4 + longest_label + 1 + MIN_REASON + 1;
    (width > taken + chip).then(|| width - 1 - chip..width - 1)
}

/// The agent-state sidebar widget.
pub struct Sidebar<'a> {
    entries: &'a [SidebarEntry],
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_auto: Option<usize>,
    hovered_window: Option<usize>,
    workspaces: usize,
    active: usize,
    names: &'a [String],
    tick: u64,
}

impl<'a> Sidebar<'a> {
    /// A sidebar over `entries`, highlighting `selected` when given and
    /// giving `hovered` (the card under the mouse) a dim marker.
    /// `workspaces` is the session's window count; workspace headers are
    /// shown only when there is more than one, with the `active` window's
    /// header lit. `tick` animates the working spinner.
    pub fn new(
        entries: &'a [SidebarEntry],
        selected: Option<usize>,
        hovered: Option<usize>,
        workspaces: usize,
        tick: u64,
    ) -> Self {
        Sidebar {
            entries,
            selected,
            hovered,
            hovered_auto: None,
            hovered_window: None,
            workspaces,
            active: 0,
            names: &[],
            tick,
        }
    }

    /// The entry index whose `auto` chip is under the mouse, for hover
    /// highlighting.
    pub fn hovered_auto(mut self, index: Option<usize>) -> Self {
        self.hovered_auto = index;
        self
    }

    /// Display names for the workspace headers, one per window — a manual
    /// name or a live terminal title, already resolved by the caller.
    /// Windows past the slice's end fall back to `workspace N`.
    pub fn names(mut self, names: &'a [String]) -> Self {
        self.names = names;
        self
    }

    /// The active window, lighting its workspace header.
    pub fn active(mut self, window: usize) -> Self {
        self.active = window;
        self
    }

    /// The workspace header under the mouse, for hover highlighting.
    pub fn hovered_window(mut self, window: Option<usize>) -> Self {
        self.hovered_window = window;
        self
    }

    /// Count of entries currently in the blocked state.
    fn blocked_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == AgentState::Blocked)
            .count()
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height == 0 {
            return;
        }
        let width = usize::from(area.width);
        let bottom = area.y + area.height;
        let mut y = area.y;

        // Quiet header: lowercase, dim; the blocked count appears on the
        // right only when someone actually needs you.
        let blocked = self.blocked_count();
        buf.set_stringn(area.x + 1, y, "agents", width.saturating_sub(1), muted());
        if blocked > 0 {
            let summary = format!("{blocked} blocked");
            let len = summary.chars().count() as u16;
            if len + 2 < area.width {
                buf.set_string(
                    area.x + area.width - 1 - len,
                    y,
                    &summary,
                    Style::default()
                        .fg(state_color(AgentState::Blocked))
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
        y += 2;

        for row in sidebar_rows(self.entries, self.workspaces) {
            if y >= bottom {
                break;
            }
            match row {
                SidebarRow::Header(window) => {
                    // The active workspace's header is lit; hovering any
                    // header lights it as a click target.
                    let mut style = if window == self.active {
                        Style::default()
                            .fg(crate::style::ACCENT)
                            .add_modifier(Modifier::ITALIC)
                    } else {
                        muted().add_modifier(Modifier::ITALIC)
                    };
                    if self.hovered_window == Some(window) {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    let label = match self.names.get(window) {
                        Some(name) => truncate(name, width.saturating_sub(2)),
                        None => format!("workspace {}", window + 1),
                    };
                    buf.set_stringn(area.x + 1, y, label, width.saturating_sub(1), style);
                }
                SidebarRow::Empty(_) => {
                    buf.set_stringn(area.x + 4, y, "no agents", width.saturating_sub(4), muted());
                }
                SidebarRow::EntryName(index) => {
                    let entry = &self.entries[index];
                    let selected = self.selected == Some(index);
                    let marker_style = Style::default().fg(crate::style::ACCENT);
                    if selected {
                        buf.set_string(area.x, y, "❯", marker_style);
                    } else if self.hovered == Some(index) {
                        // Hover affordance: a quiet marker where selection's
                        // ❯ goes.
                        buf.set_string(area.x, y, "❯", muted());
                    }

                    // Glyph + agent name (bold; accented when selected), age
                    // right-aligned and dim.
                    buf.set_string(
                        area.x + 2,
                        y,
                        state_glyph(entry.state, self.tick),
                        Style::default().fg(state_color(entry.state)),
                    );
                    let age = entry.age.map(format_age).unwrap_or_default();
                    let name_width = width.saturating_sub(4).saturating_sub(age.len() + 1);
                    let name_style = if selected {
                        Style::default()
                            .fg(crate::style::ACCENT)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    };
                    buf.set_string(
                        area.x + 4,
                        y,
                        truncate(&entry.agent, name_width),
                        name_style,
                    );
                    if !age.is_empty() {
                        let x = area.x + area.width - 1 - age.len() as u16;
                        buf.set_string(x, y, &age, muted());
                    }
                }
                SidebarRow::EntryDetail(index) => {
                    // The state word in its own color — the signal — the
                    // reason dimmed after it, and the `auto` chip pinned at
                    // the right edge: the per-pane auto-approve toggle,
                    // muted until it's on so every card shows where to
                    // click.
                    let entry = &self.entries[index];
                    let label = state_label(entry.state);
                    buf.set_stringn(
                        area.x + 4,
                        y,
                        label,
                        width.saturating_sub(4),
                        Style::default().fg(state_color(entry.state)),
                    );
                    let chip = auto_chip_cols(area.width);
                    if let Some(reason) = &entry.reason {
                        // The reason yields to the chip: its budget ends a
                        // gutter column short of it (or one column in from
                        // the edge when no chip fits).
                        let used = 4 + label.chars().count();
                        let end = chip.as_ref().map_or(width.saturating_sub(1), |cols| {
                            usize::from(cols.start).saturating_sub(1)
                        });
                        let rest = end.saturating_sub(used);
                        if rest > 4 {
                            buf.set_stringn(
                                area.x + used as u16,
                                y,
                                format!(" · {}", truncate(reason, rest.saturating_sub(3))),
                                rest,
                                muted(),
                            );
                        }
                    }
                    if let Some(cols) = chip {
                        // On mirrors the layout switcher's active word —
                        // accent + bold — so the state survives terminals
                        // that drop color; off is plain muted chrome.
                        let mut style = if entry.auto_approve {
                            Style::default()
                                .fg(crate::style::ACCENT)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            muted()
                        };
                        if self.hovered_auto == Some(index) {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        buf.set_string(area.x + cols.start, y, AUTO_CHIP, style);
                    }
                }
                SidebarRow::EntryTelemetry(index) => {
                    // Badge line under the detail row, same indent: model,
                    // context %, cost, rate limit — formatted and colored by
                    // `telemetry_line` (the context badge escalates through
                    // the state colors as it runs out).
                    let entry = &self.entries[index];
                    if let Some(telemetry) = &entry.telemetry {
                        // Card indent + one right-margin column, like the
                        // rows above.
                        let budget = width.saturating_sub(4).saturating_sub(1);
                        let line = truncate_line(telemetry_line(telemetry), budget);
                        buf.set_line(area.x + 4, y, &line, area.width.saturating_sub(4));
                    }
                }
                SidebarRow::Blank => {}
            }
            y += 1;
        }
    }
}

/// Compact age for the sidebar: seconds under a minute, then minutes, then
/// hours.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// [`truncate`] for a styled line, keeping each span's style: a cut is
/// marked with the same trailing `…` as the plain-text rows. A hard clip
/// would leave a badge reading as a smaller number than it is
/// (`$12.34` → `$12`), which misleads instead of signalling "narrow".
/// Budgets by display cells, not chars — the buffer clips by cells, and a
/// double-width char (the feed copies `display_name` verbatim) must not
/// push the `…` off the edge.
fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let mut budget = width.saturating_sub(1);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for span in line.spans {
        if budget == 0 {
            break;
        }
        if span.width() <= budget {
            budget -= span.width();
            spans.push(span);
        } else {
            // Cut inside the span on a char boundary, counting cells: a
            // wide char that doesn't fit whole is dropped whole.
            let mut end = 0;
            let mut used = 0;
            for (at, ch) in span.content.char_indices() {
                let next = at + ch.len_utf8();
                let cells = Span::raw(&span.content[at..next]).width();
                if used + cells > budget {
                    break;
                }
                used += cells;
                end = next;
            }
            spans.push(Span::styled(span.content[..end].to_string(), span.style));
            budget = 0;
        }
    }
    spans.push(Span::styled("…", muted()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use roster_core::SplitDirection;

    fn buffer_row(buf: &Buffer, y: u16) -> String {
        let area = *buf.area();
        (area.x..area.right())
            .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Build a session with three agent panes and one plain shell pane.
    fn populated_session(now: Instant) -> (Session, Vec<PaneId>) {
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        let c = session.split(b, SplitDirection::Vertical).unwrap();
        let d = session.split(c, SplitDirection::Vertical).unwrap();

        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.pane_mut(c).unwrap().command = Some("claude".into());
        session.pane_mut(d).unwrap().command = Some("zsh".into());

        session.set_reading(
            a,
            AgentState::Working,
            Some("running tests".into()),
            now - Duration::from_secs(5),
        );
        session.set_reading(
            b,
            AgentState::Blocked,
            Some("Approve this command?".into()),
            now - Duration::from_secs(30),
        );
        session.set_reading(
            c,
            AgentState::Done,
            Some("finished".into()),
            now - Duration::from_secs(2),
        );
        (session, vec![a, b, c, d])
    }

    #[test]
    fn entries_skip_non_agent_panes() {
        let now = Instant::now();
        let (session, ids) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|e| e.pane != ids[3]));
    }

    #[test]
    fn entries_sort_blocked_then_done_then_working() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let states: Vec<AgentState> = entries.iter().map(|e| e.state).collect();
        assert_eq!(
            states,
            vec![AgentState::Blocked, AgentState::Done, AgentState::Working]
        );
        assert_eq!(entries[0].agent, "claude-code");
        assert_eq!(entries[0].age, Some(Duration::from_secs(30)));
    }

    #[test]
    fn longer_waiting_rows_lead_within_a_state() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(
            a,
            AgentState::Blocked,
            Some("q1".into()),
            now - Duration::from_secs(3),
        );
        session.set_reading(
            b,
            AgentState::Blocked,
            Some("q2".into()),
            now - Duration::from_secs(60),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Both panes run claude; the longer-waiting one (pane b, 60s) leads.
        assert_eq!(entries[0].pane, b);
        assert_eq!(entries[1].pane, a);
    }

    #[test]
    fn entries_group_and_order_by_workspace() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Blocked, Some("q".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Window 0's agent comes before window 1's, even though window 1's is
        // blocked — grouping is by workspace first.
        assert_eq!(entries[0].window, 0);
        assert_eq!(entries[0].agent, "claude-code");
        assert_eq!(entries[1].window, 1);
        assert_eq!(entries[1].agent, "claude-code");
    }

    #[test]
    fn format_age_scales_units() {
        assert_eq!(format_age(Duration::from_secs(12)), "12s");
        assert_eq!(format_age(Duration::from_secs(90)), "1m");
        assert_eq!(format_age(Duration::from_secs(3700)), "1h");
        assert_eq!(format_age(Duration::ZERO), "0s");
    }

    #[test]
    fn selection_wraps_both_ways_and_activates() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut state = SidebarState::new();
        assert_eq!(state.selected(entries.len()), Some(0));

        state.select_prev(entries.len());
        assert_eq!(state.selected(entries.len()), Some(2));
        state.select_next(entries.len());
        assert_eq!(state.selected(entries.len()), Some(0));

        assert_eq!(
            state.activate(&entries),
            Some(Message::JumpToPane(entries[0].pane))
        );
        assert_eq!(SidebarState::new().selected(0), None);
        assert_eq!(SidebarState::new().activate(&[]), None);
    }

    #[test]
    fn renders_header_and_spaced_state_colored_cards() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);

        // Quiet lowercase header with the blocked count on the right.
        let header = buffer_row(&buf, 0);
        assert!(header.starts_with(" agents"), "header: {header}");
        assert!(header.ends_with("1 blocked"), "header: {header}");

        // Cards start after a blank row: the blocked card first (glyph, bold
        // name, right-aligned age; state word + dim reason below), then a
        // blank spacer before the next card.
        let name_row = buffer_row(&buf, 2);
        assert!(name_row.starts_with("  ◉ claude-code"), "row: {name_row}");
        assert!(name_row.ends_with("30s"), "row: {name_row}");
        let detail_row = buffer_row(&buf, 3);
        assert!(
            detail_row.starts_with("    blocked · Approve"),
            "row: {detail_row}"
        );
        assert_eq!(buffer_row(&buf, 4), "");
        // The second card (after the spacer) is the done pane; assert its
        // detail row — the state + reason still distinguishes cards now that
        // every card shares the name "claude-code".
        assert!(
            buffer_row(&buf, 6).starts_with("    done · finished"),
            "second card is the done pane: {}",
            buffer_row(&buf, 6)
        );

        // The state word is colored, the reason after it is not.
        assert_eq!(
            buf.cell((4, 3)).unwrap().style().fg,
            Some(state_color(AgentState::Blocked))
        );
    }

    #[test]
    fn auto_chip_cols_right_align_and_guard_narrow_widths() {
        // Right-aligned one column in from the edge.
        assert_eq!(auto_chip_cols(31), Some(26..30));
        assert_eq!(auto_chip_cols(40), Some(35..39));
        // One width-only threshold — sized to the longest state word plus
        // a reason reserve — so the chip never flickers as a card's state
        // changes and never starves the reason of its sliver.
        assert_eq!(auto_chip_cols(26), Some(21..25));
        assert_eq!(auto_chip_cols(25), None);
        assert_eq!(auto_chip_cols(0), None);
    }

    #[test]
    fn auto_chip_sits_on_every_card_accent_on_muted_off() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let mut entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Auto-approve the first (blocked) card; leave the rest off.
        entries[0].auto_approve = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        // The chip ends every card's detail row — the toggle is always
        // visible, not just when it's on.
        let detail = buffer_row(&buf, 3);
        assert!(detail.ends_with("auto"), "chip missing: {detail}");
        let other = buffer_row(&buf, 6);
        assert!(other.ends_with("auto"), "chip missing: {other}");
        // Accent + bold when on — mirroring the layout switcher's active
        // word, and legible without color — plain muted when off.
        let cols = auto_chip_cols(40).unwrap();
        let on = buf.cell((cols.start, 3)).unwrap().style();
        assert_eq!(on.fg, Some(crate::style::ACCENT));
        assert!(on.add_modifier.contains(Modifier::BOLD));
        let off = buf.cell((cols.start, 6)).unwrap().style();
        assert_eq!(off.fg, muted().fg);
        assert!(!off.add_modifier.contains(Modifier::BOLD));
        // A gutter column separates the truncated reason from the chip.
        assert_eq!(buf.cell((cols.start - 1, 3)).unwrap().symbol(), " ");
    }

    #[test]
    fn auto_chip_hover_inverts_the_chip() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .hovered_auto(Some(0))
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        let cols = auto_chip_cols(40).unwrap();
        assert!(buf
            .cell((cols.start, 3))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
        // Hovering one chip leaves the others plain.
        assert!(!buf
            .cell((cols.start, 6))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn auto_chip_gives_way_to_the_reason_on_a_narrow_sidebar() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // One column below the chip's threshold: the reason — the signal —
        // still renders; the chip is what yields.
        let mut buf = Buffer::empty(Rect::new(0, 0, 25, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 25, 14), &mut buf);
        let detail = buffer_row(&buf, 3);
        assert!(
            detail.starts_with("    blocked · Approve"),
            "reason lost: {detail}"
        );
        for y in 0..14 {
            let row = buffer_row(&buf, y);
            assert!(!row.contains("auto"), "chip on a narrow row: {row}");
        }
    }

    #[test]
    fn telemetry_cards_grow_a_badge_line_and_bridge_less_cards_do_not() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        // Feed telemetry to the blocked pane only; the others stay two-line.
        session.set_telemetry(
            ids[1],
            Some(Telemetry {
                model: Some("Opus".into()),
                context_pct: Some(62.0),
                cost_usd: Some(1.23),
                rate_limit: None,
            }),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);

        // The row plan grows the telemetry row for that card alone.
        let rows = sidebar_rows(&entries, 1);
        assert_eq!(
            rows,
            vec![
                SidebarRow::EntryName(0),
                SidebarRow::EntryDetail(0),
                SidebarRow::EntryTelemetry(0),
                SidebarRow::Blank,
                SidebarRow::EntryName(1),
                SidebarRow::EntryDetail(1),
                SidebarRow::Blank,
                SidebarRow::EntryName(2),
                SidebarRow::EntryDetail(2),
                SidebarRow::Blank,
            ]
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 40, 16), &mut buf);
        // The badge line sits under the blocked card's detail row, at the
        // card indent, with the muted separators.
        assert_eq!(buffer_row(&buf, 4), "    Opus · 62% context · $1.23");
        // Muted chrome for the model badge; the healthy context badge too.
        assert_eq!(buf.cell((4, 4)).unwrap().style().fg, muted().fg);
        // The next card starts after one blank, one row lower than before.
        assert_eq!(buffer_row(&buf, 5), "");
        assert!(
            buffer_row(&buf, 6).starts_with("  "),
            "second card shifted down: {}",
            buffer_row(&buf, 6)
        );
        assert!(
            buffer_row(&buf, 7).starts_with("    done · finished"),
            "second card detail: {}",
            buffer_row(&buf, 7)
        );
    }

    #[test]
    fn narrow_badge_lines_truncate_with_an_ellipsis_not_a_hard_clip() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        session.set_telemetry(
            ids[1],
            Some(Telemetry {
                model: Some("Opus".into()),
                context_pct: Some(62.0),
                cost_usd: Some(12.34),
                rate_limit: None,
            }),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Too narrow for the whole line: the cut is marked with the same
        // trailing … the name and reason rows use, and no partial cost
        // survives to read as a smaller number than it is.
        let mut buf = Buffer::empty(Rect::new(0, 0, 26, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 26, 14), &mut buf);
        let row = buffer_row(&buf, 4);
        assert!(row.ends_with('…'), "cut not marked: {row}");
        assert!(
            !row.contains('$'),
            "clipped cost reads as a fabricated number: {row}"
        );

        // Cell-width budgeting: a double-width model name must not push
        // the … past the buffer's clip and hard-cut a number after all.
        session.set_telemetry(
            ids[1],
            Some(Telemetry {
                model: Some("私のモデル".into()),
                context_pct: Some(62.0),
                cost_usd: Some(12.34),
                rate_limit: None,
            }),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 26, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 26, 14), &mut buf);
        let row = buffer_row(&buf, 4);
        assert!(row.ends_with('…'), "wide-char cut not marked: {row}");
        assert!(
            !row.contains('$'),
            "wide-char clip fabricated a number: {row}"
        );
    }

    #[test]
    fn critical_context_badge_renders_bold_blocked_red_on_the_card() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        session.set_telemetry(
            ids[0],
            Some(Telemetry {
                context_pct: Some(5.0),
                ..Telemetry::default()
            }),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // ids[0] is the working pane: third card in sort order, so its
        // telemetry row is the last card's third line.
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 40, 16), &mut buf);
        let badge_row = (0..16)
            .find(|y| buffer_row(&buf, *y).contains("5% context"))
            .expect("critical badge rendered");
        let x = buffer_row(&buf, badge_row).find("5%").unwrap() as u16;
        let style = buf.cell((x, badge_row)).unwrap().style();
        assert_eq!(style.fg, Some(state_color(AgentState::Blocked)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn glyphs_carry_state_color_and_working_spins() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // Blocked ring at the first card's glyph column.
        assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "◉");
        assert_eq!(
            buf.cell((2, 2)).unwrap().style().fg,
            Some(state_color(AgentState::Blocked))
        );

        // The working card's glyph changes with the tick.
        let glyph_at = |tick: u64| -> String {
            let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
            Sidebar::new(&entries, None, None, session.window_count(), tick)
                .render(Rect::new(0, 0, 32, 14), &mut buf);
            // Working card is third: rows 8/9; glyph at (2, 8).
            buf.cell((2, 8)).unwrap().symbol().to_string()
        };
        assert_ne!(glyph_at(0), glyph_at(1));
    }

    #[test]
    fn workspace_headers_appear_with_multiple_windows() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Working, Some("go".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 16), &mut buf);

        let rows: Vec<String> = (0..16).map(|y| buffer_row(&buf, y)).collect();
        assert!(
            rows.iter().any(|r| r.trim() == "workspace 1"),
            "rows: {rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.trim() == "workspace 2"),
            "rows: {rows:#?}"
        );
    }

    #[test]
    fn selected_card_shows_accent_marker() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, Some(0), None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // Marker on the selected card's name row; name in the accent color.
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "❯");
        assert_eq!(
            buf.cell((0, 2)).unwrap().style().fg,
            Some(crate::style::ACCENT)
        );
        assert_eq!(
            buf.cell((4, 2)).unwrap().style().fg,
            Some(crate::style::ACCENT)
        );
        // Unselected cards keep the default name color.
        assert_ne!(
            buf.cell((4, 5)).unwrap().style().fg,
            Some(crate::style::ACCENT)
        );
    }
}
