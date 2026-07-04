//! The agent launcher: a centered modal for starting agents at runtime.
//!
//! Lists the configured agents plus a shell, filters as you type, and falls
//! back to running whatever you typed — so known agents are two keystrokes
//! and anything else is still one command line away. Selection produces a
//! [`Message`]-style intent; the binary owns the actual spawn.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use roster_detect::Detector;

use crate::style::ACCENT;

/// One launchable item: a display name and the command it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchItem {
    /// Display name, e.g. `claude-code` or `shell`.
    pub name: String,
    /// The shell command to run.
    pub command: String,
}

/// Build the launcher's item list: every configured agent (launched via its
/// first `match_command` binary) plus the user's shell.
pub fn launch_items(detector: &Detector, shell: &str) -> Vec<LaunchItem> {
    let mut items: Vec<LaunchItem> = detector
        .agents()
        .filter_map(|agent| {
            Some(LaunchItem {
                name: agent.name.clone(),
                command: agent.match_command.first()?.clone(),
            })
        })
        .collect();
    items.push(LaunchItem {
        name: "shell".to_string(),
        command: shell.to_string(),
    });
    items
}

/// Launcher input state: the typed filter and the selected row.
#[derive(Clone, Debug, Default)]
pub struct LauncherState {
    input: String,
    selected: usize,
}

impl LauncherState {
    /// Fresh state: empty filter, first row selected.
    pub fn new() -> Self {
        LauncherState::default()
    }

    /// The typed text.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Append a typed character and reset the selection to the best match.
    pub fn push(&mut self, c: char) {
        self.input.push(c);
        self.selected = 0;
    }

    /// Delete the last typed character.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.selected = 0;
    }

    /// Items whose name or command contains the typed text.
    pub fn filtered<'a>(&self, items: &'a [LaunchItem]) -> Vec<&'a LaunchItem> {
        let needle = self.input.to_lowercase();
        items
            .iter()
            .filter(|item| {
                item.name.to_lowercase().contains(&needle)
                    || item.command.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Move the selection down, wrapping within the filtered list.
    pub fn select_next(&mut self, items: &[LaunchItem]) {
        let len = self.filtered(items).len();
        if len > 0 {
            self.selected = (self.selected.min(len - 1) + 1) % len;
        }
    }

    /// Move the selection up, wrapping within the filtered list.
    pub fn select_prev(&mut self, items: &[LaunchItem]) {
        let len = self.filtered(items).len();
        if len > 0 {
            let current = self.selected.min(len - 1);
            self.selected = (current + len - 1) % len;
        }
    }

    /// The selected index within the filtered list, if it is non-empty.
    pub fn selected(&self, items: &[LaunchItem]) -> Option<usize> {
        let len = self.filtered(items).len();
        (len > 0).then(|| self.selected.min(len - 1))
    }

    /// The command to run on enter: the selected match, or the raw typed
    /// text when nothing matches. `None` when there is nothing to run.
    pub fn command(&self, items: &[LaunchItem]) -> Option<String> {
        let filtered = self.filtered(items);
        if let Some(index) = self.selected(items) {
            return Some(filtered[index].command.clone());
        }
        let typed = self.input.trim();
        (!typed.is_empty()).then(|| typed.to_string())
    }
}

/// The launcher modal widget, drawn centered inside a given area.
pub struct Launcher<'a> {
    items: &'a [LaunchItem],
    state: &'a LauncherState,
}

impl<'a> Launcher<'a> {
    /// A launcher over `items` with the current input `state`.
    pub fn new(items: &'a [LaunchItem], state: &'a LauncherState) -> Self {
        Launcher { items, state }
    }

    /// The centered rect the modal occupies within `area`.
    pub fn modal_rect(&self, area: Rect) -> Rect {
        let width = 44u16.min(area.width.saturating_sub(2)).max(20);
        let rows = self.state.filtered(self.items).len() as u16;
        // border + title + input + rows + border
        let height = (rows + 4).clamp(5, area.height.saturating_sub(2).max(5));
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 3;
        Rect::new(x, y, width, height)
    }
}

impl Widget for Launcher<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let modal = self.modal_rect(area);
        if modal.width < 8 || modal.height < 5 {
            return;
        }
        fill(buf, modal);
        frame(buf, modal, " new agent ");

        let inner_x = modal.x + 2;
        let inner_w = usize::from(modal.width.saturating_sub(4));
        let mut y = modal.y + 1;

        // Input line, prompt-style.
        let input = format!("❯ {}", self.state.input());
        buf.set_stringn(
            inner_x,
            y,
            &input,
            inner_w,
            Style::default().add_modifier(Modifier::BOLD),
        );
        y += 1;

        let filtered = self.state.filtered(self.items);
        let selected = self.state.selected(self.items);
        if filtered.is_empty() {
            let typed = self.state.input().trim().to_string();
            let hint = if typed.is_empty() {
                "type a command…".to_string()
            } else {
                format!("↵ run: {typed}")
            };
            buf.set_stringn(
                inner_x,
                y,
                hint,
                inner_w,
                Style::default().add_modifier(Modifier::DIM),
            );
            return;
        }
        for (index, item) in filtered.iter().enumerate() {
            if y >= modal.y + modal.height - 1 {
                break;
            }
            let marker = if selected == Some(index) { "❯" } else { " " };
            buf.set_stringn(
                inner_x,
                y,
                format!("{marker} {}", item.name),
                inner_w,
                if selected == Some(index) {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            );
            let cmd = &item.command;
            let cmd_len = cmd.chars().count() as u16;
            if cmd_len + 4 < modal.width {
                buf.set_string(
                    modal.x + modal.width - 2 - cmd_len,
                    y,
                    cmd,
                    Style::default().add_modifier(Modifier::DIM),
                );
            }
            if selected == Some(index) {
                buf.set_style(
                    Rect::new(modal.x + 1, y, modal.width - 2, 1),
                    Style::default().add_modifier(Modifier::REVERSED),
                );
            }
            y += 1;
        }
    }
}

/// Blank the modal's cells so panes underneath don't bleed through.
fn fill(buf: &mut Buffer, rect: Rect) {
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }
}

/// Draw a rounded border with a title, in the accent color.
fn frame(buf: &mut Buffer, rect: Rect, title: &str) {
    let style = Style::default().fg(ACCENT);
    let (left, right, top, bottom) = (
        rect.x,
        rect.x + rect.width - 1,
        rect.y,
        rect.y + rect.height - 1,
    );
    for x in left..=right {
        for (y, ch) in [(top, '─'), (bottom, '─')] {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
    }
    for y in top..=bottom {
        for (x, ch) in [(left, '│'), (right, '│')] {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
    }
    for (x, y, ch) in [
        (left, top, '╭'),
        (right, top, '╮'),
        (left, bottom, '╰'),
        (right, bottom, '╯'),
    ] {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
    buf.set_stringn(
        rect.x + 2,
        rect.y,
        title,
        usize::from(rect.width.saturating_sub(4)),
        style.add_modifier(Modifier::BOLD),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<LaunchItem> {
        launch_items(&Detector::builtin(), "/bin/zsh")
    }

    #[test]
    fn items_cover_agents_and_shell() {
        let items = items();
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["aider", "claude-code", "codex", "shell"]);
        assert_eq!(items[1].command, "claude");
        assert_eq!(items[3].command, "/bin/zsh");
    }

    #[test]
    fn typing_filters_and_enter_picks_the_match() {
        let items = items();
        let mut state = LauncherState::new();
        for c in "cla".chars() {
            state.push(c);
        }
        let filtered = state.filtered(&items);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "claude-code");
        assert_eq!(state.command(&items).as_deref(), Some("claude"));
    }

    #[test]
    fn unmatched_input_runs_verbatim() {
        let items = items();
        let mut state = LauncherState::new();
        for c in "npx some-agent --yolo".chars() {
            state.push(c);
        }
        assert!(state.filtered(&items).is_empty());
        assert_eq!(
            state.command(&items).as_deref(),
            Some("npx some-agent --yolo")
        );
    }

    #[test]
    fn empty_input_defaults_to_first_item() {
        let items = items();
        let state = LauncherState::new();
        assert_eq!(state.command(&items).as_deref(), Some("aider"));
    }

    #[test]
    fn selection_wraps_and_backspace_refilters() {
        let items = items();
        let mut state = LauncherState::new();
        state.select_prev(&items);
        assert_eq!(state.selected(&items), Some(3));
        state.select_next(&items);
        assert_eq!(state.selected(&items), Some(0));

        state.push('z');
        state.push('z');
        assert!(state.filtered(&items).is_empty());
        assert_eq!(state.command(&items).as_deref(), Some("zz"));
        state.backspace();
        state.backspace();
        assert_eq!(state.filtered(&items).len(), 4);
    }

    #[test]
    fn whitespace_only_input_launches_nothing() {
        let items = items();
        let mut spaces = LauncherState::new();
        for c in "   ".chars() {
            spaces.push(c);
        }
        assert!(spaces.filtered(&items).is_empty());
        assert_eq!(spaces.command(&items), None);

        // Trailing whitespace around a real command is trimmed, not fatal.
        let mut padded = LauncherState::new();
        for c in "zzz  ".chars() {
            padded.push(c);
        }
        assert_eq!(padded.command(&items).as_deref(), Some("zzz"));
    }

    #[test]
    fn modal_renders_centered_with_items_and_input() {
        let items = items();
        let mut state = LauncherState::new();
        state.push('c');
        let launcher = Launcher::new(&items, &state);
        let area = Rect::new(0, 0, 80, 24);
        let modal = launcher.modal_rect(area);
        assert!(modal.x > 10 && modal.x + modal.width < 70);

        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state).render(area, &mut buf);
        let row = |y: u16| -> String {
            (0..80u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        };
        let all: String = (0..24).map(|y| row(y) + "\n").collect();
        assert!(all.contains("new agent"), "missing title:\n{all}");
        assert!(all.contains("❯ c"), "missing input line:\n{all}");
        assert!(all.contains("claude-code"), "missing item:\n{all}");
        assert!(all.contains("codex"), "missing item:\n{all}");
    }
}
