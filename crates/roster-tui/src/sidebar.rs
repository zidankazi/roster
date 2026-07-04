//! The agent-state sidebar: agents grouped by workspace, each rendered as a
//! two-line card — a colored status glyph, the agent name, and its age on
//! top; the state and its reason below. Blocked and done float to the top of
//! each workspace so the agents that need you are always in view.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use roster_core::{AgentState, PaneId, Session};
use roster_detect::Detector;

use crate::style::{state_color, state_glyph, state_label};

/// One sidebar row: an agent pane and everything shown about it.
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// The agent-state sidebar widget.
pub struct Sidebar<'a> {
    entries: &'a [SidebarEntry],
    selected: Option<usize>,
    workspaces: usize,
}

impl<'a> Sidebar<'a> {
    /// A sidebar over `entries`, highlighting `selected` when given.
    /// `workspaces` is the session's window count; workspace headers are
    /// shown only when there is more than one.
    pub fn new(entries: &'a [SidebarEntry], selected: Option<usize>, workspaces: usize) -> Self {
        Sidebar {
            entries,
            selected,
            workspaces,
        }
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

        // Title: "roster" left, agent count right (blocked count in red).
        let blocked = self.blocked_count();
        let summary = if blocked > 0 {
            format!("{} blocked", blocked)
        } else {
            format!("{} agents", self.entries.len())
        };
        let summary_style = if blocked > 0 {
            Style::default()
                .fg(state_color(AgentState::Blocked))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        draw_split(
            buf,
            Rect::new(area.x, y, area.width, 1),
            (" roster", Style::default().add_modifier(Modifier::BOLD)),
            (&summary, summary_style),
        );
        y += 1;
        if y < bottom {
            buf.set_string(
                area.x,
                y,
                "─".repeat(width),
                Style::default().add_modifier(Modifier::DIM),
            );
            y += 1;
        }

        let mut last_window: Option<usize> = None;
        for (index, entry) in self.entries.iter().enumerate() {
            if y >= bottom {
                break;
            }
            if self.workspaces > 1 && last_window != Some(entry.window) {
                buf.set_stringn(
                    area.x,
                    y,
                    format!(" workspace {}", entry.window + 1),
                    width,
                    Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC),
                );
                last_window = Some(entry.window);
                y += 1;
                if y >= bottom {
                    break;
                }
            }

            let card_top = y;
            // Line 1: glyph + agent name, age right-aligned.
            buf.set_string(
                area.x + 1,
                y,
                state_glyph(entry.state),
                Style::default().fg(state_color(entry.state)),
            );
            let age = entry.age.map(format_age).unwrap_or_default();
            let name_width = width.saturating_sub(3).saturating_sub(age.len() + 1);
            buf.set_string(
                area.x + 3,
                y,
                truncate(&entry.agent, name_width),
                Style::default().add_modifier(Modifier::BOLD),
            );
            if !age.is_empty() {
                let x = area.x + area.width - age.len() as u16;
                buf.set_string(x, y, &age, Style::default().add_modifier(Modifier::DIM));
            }
            y += 1;

            // Line 2: state word, then reason if present.
            if y < bottom {
                let mut detail = state_label(entry.state).to_string();
                if let Some(reason) = &entry.reason {
                    detail.push_str(" · ");
                    detail.push_str(reason);
                }
                buf.set_stringn(
                    area.x + 3,
                    y,
                    truncate(&detail, width.saturating_sub(3)),
                    width.saturating_sub(3),
                    Style::default().add_modifier(Modifier::DIM),
                );
                y += 1;
            }

            if self.selected == Some(index) {
                for row in card_top..y {
                    buf.set_style(
                        Rect::new(area.x, row, area.width, 1),
                        Style::default().add_modifier(Modifier::REVERSED),
                    );
                }
            }
        }
    }
}

/// Draw `left` from the left edge and `right` flush to the right edge of
/// `span`'s first row, each in its own style.
fn draw_split(buf: &mut Buffer, span: Rect, left: (&str, Style), right: (&str, Style)) {
    buf.set_stringn(span.x, span.y, left.0, usize::from(span.width), left.1);
    let right_len = right.0.chars().count() as u16;
    if right_len < span.width {
        buf.set_string(span.x + span.width - right_len, span.y, right.0, right.1);
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
    fn entries_group_and_order_by_workspace() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("codex".into());
        session.set_reading(b, AgentState::Blocked, Some("q".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Window 0's agent comes before window 1's, even though window 1's is
        // blocked — grouping is by workspace first.
        assert_eq!(entries[0].window, 0);
        assert_eq!(entries[0].agent, "claude-code");
        assert_eq!(entries[1].window, 1);
        assert_eq!(entries[1].agent, "codex");
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
    fn renders_title_rule_and_agent_cards() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 12));
        Sidebar::new(&entries, None, session.window_count())
            .render(Rect::new(0, 0, 32, 12), &mut buf);

        // Title with a blocked-count summary, then a rule.
        let title = buffer_row(&buf, 0);
        assert!(title.starts_with(" roster"), "title: {title}");
        assert!(title.ends_with("1 blocked"), "title: {title}");
        assert!(buffer_row(&buf, 1).starts_with("──"));

        // First card: blocked codex on top (glyph, name, age; then detail).
        assert_eq!(buffer_row(&buf, 2), " ● codex                     30s");
        assert_eq!(buffer_row(&buf, 3), "   blocked · Approve this comma…");
    }

    #[test]
    fn dot_carries_the_state_color() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 12));
        Sidebar::new(&entries, None, session.window_count())
            .render(Rect::new(0, 0, 32, 12), &mut buf);
        // The glyph sits at column 1 of the blocked card's first row (row 2).
        assert_eq!(buf.cell((1, 2)).unwrap().symbol(), "●");
        assert_eq!(
            buf.cell((1, 2)).unwrap().style().fg,
            Some(state_color(AgentState::Blocked))
        );
    }

    #[test]
    fn workspace_headers_appear_with_multiple_windows() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("codex".into());
        session.set_reading(b, AgentState::Working, Some("go".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, session.window_count())
            .render(Rect::new(0, 0, 32, 14), &mut buf);

        let rows: Vec<String> = (0..14).map(|y| buffer_row(&buf, y)).collect();
        assert!(rows.iter().any(|r| r == " workspace 1"), "rows: {rows:#?}");
        assert!(rows.iter().any(|r| r == " workspace 2"), "rows: {rows:#?}");
    }

    #[test]
    fn selected_card_is_reversed_on_both_lines() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 12));
        Sidebar::new(&entries, Some(0), session.window_count())
            .render(Rect::new(0, 0, 32, 12), &mut buf);
        // Card 0 occupies rows 2 and 3.
        assert!(buf
            .cell((0, 2))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
        assert!(buf
            .cell((0, 3))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
        assert!(!buf
            .cell((0, 4))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::REVERSED));
    }
}
