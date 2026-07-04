//! The agent-state sidebar: one row per agent, blocked and done floated to
//! the top, each row showing color, label, reason, and age.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use roster_core::{AgentState, PaneId, Session};
use roster_detect::Detector;

use crate::style::{state_color, state_label};

/// One sidebar row: an agent pane and everything shown about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarEntry {
    /// The pane this row describes.
    pub pane: PaneId,
    /// The agent's config name, e.g. `claude-code`.
    pub agent: String,
    /// The committed state.
    pub state: AgentState,
    /// Why — shown after the state label.
    pub reason: Option<String>,
    /// Time since the state last changed, if it ever has.
    pub age: Option<Duration>,
}

/// Build the sidebar rows from the session: every pane whose command
/// identifies as a configured agent, sorted so blocked and done float up and
/// the longest-waiting rows lead within each state.
pub fn sidebar_entries(session: &Session, detector: &Detector, now: Instant) -> Vec<SidebarEntry> {
    let mut entries: Vec<SidebarEntry> = session
        .panes()
        .into_iter()
        .filter_map(|pane| {
            let command = pane.command.as_deref()?;
            let kind = detector.identify(command)?;
            Some(SidebarEntry {
                pane: pane.id,
                agent: detector.agent(kind).name.clone(),
                state: pane.state,
                reason: pane.reason.clone(),
                age: pane.last_change.map(|at| now.saturating_duration_since(at)),
            })
        })
        .collect();
    entries.sort_by_key(|e| {
        (
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

/// The sidebar widget: renders entries top to bottom, one row each.
pub struct Sidebar<'a> {
    entries: &'a [SidebarEntry],
    selected: Option<usize>,
}

impl<'a> Sidebar<'a> {
    /// A sidebar over `entries`, highlighting `selected` when given.
    pub fn new(entries: &'a [SidebarEntry], selected: Option<usize>) -> Self {
        Sidebar { entries, selected }
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 {
            return;
        }
        for (row, entry) in self
            .entries
            .iter()
            .take(usize::from(area.height))
            .enumerate()
        {
            let y = area.y + row as u16;
            buf.set_string(
                area.x,
                y,
                "●",
                Style::default().fg(state_color(entry.state)),
            );

            let age = entry.age.map(format_age).unwrap_or_default();
            let text_width = usize::from(area.width)
                .saturating_sub(2) // dot + space
                .saturating_sub(age.len() + 1); // gap + age
            let mut text = format!("{} {}", entry.agent, state_label(entry.state));
            if let Some(reason) = &entry.reason {
                text.push_str(": ");
                text.push_str(reason);
            }
            buf.set_string(area.x + 2, y, truncate(&text, text_width), Style::default());
            if !age.is_empty() {
                let x = area.x + area.width - age.len() as u16;
                buf.set_string(x, y, &age, Style::default().add_modifier(Modifier::DIM));
            }
            if self.selected == Some(row) {
                buf.set_style(
                    Rect::new(area.x, y, area.width, 1),
                    Style::default().add_modifier(Modifier::REVERSED),
                );
            }
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
        session.pane_mut(b).unwrap().command = Some("codex".into());
        session.pane_mut(c).unwrap().command = Some("aider --model sonnet".into());
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
        assert_eq!(entries[0].agent, "codex");
        assert_eq!(entries[0].age, Some(Duration::from_secs(30)));
    }

    #[test]
    fn longer_waiting_rows_lead_within_a_state() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("codex".into());
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
        assert_eq!(entries[0].agent, "codex");
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
    fn rows_render_dot_label_reason_and_age() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 44, 4));
        Sidebar::new(&entries, None).render(Rect::new(0, 0, 44, 4), &mut buf);

        assert_eq!(
            buffer_row(&buf, 0),
            "● codex blocked: Approve this command?   30s"
        );
        assert_eq!(
            buffer_row(&buf, 1),
            "● aider done: finished                    2s"
        );
        assert_eq!(
            buffer_row(&buf, 2),
            "● claude-code working: running tests      5s"
        );
        assert_eq!(buffer_row(&buf, 3), "");
    }

    #[test]
    fn dot_carries_the_state_color() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 44, 3));
        Sidebar::new(&entries, None).render(Rect::new(0, 0, 44, 3), &mut buf);
        assert_eq!(
            buf.cell((0, 0)).unwrap().style().fg,
            Some(state_color(AgentState::Blocked))
        );
        assert_eq!(
            buf.cell((0, 1)).unwrap().style().fg,
            Some(state_color(AgentState::Done))
        );
    }

    #[test]
    fn selected_row_is_reversed() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 44, 3));
        Sidebar::new(&entries, Some(1)).render(Rect::new(0, 0, 44, 3), &mut buf);
        assert!(buf
            .cell((0, 1))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
        assert!(!buf
            .cell((0, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn long_reasons_truncate_with_ellipsis() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(
            a,
            AgentState::Blocked,
            Some("Allow edit to a very deeply nested configuration file?".into()),
            now - Duration::from_secs(9),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 1));
        Sidebar::new(&entries, None).render(Rect::new(0, 0, 30, 1), &mut buf);
        let row = buffer_row(&buf, 0);
        assert!(row.starts_with("● claude-code blocked: A"), "row: {row}");
        assert!(row.contains('…'), "row: {row}");
        assert!(row.ends_with("9s"), "row: {row}");
    }
}
