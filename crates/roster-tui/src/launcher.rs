//! The agent launcher: a centered modal for starting agents at runtime.
//!
//! Lists the configured agents, filters as you type, and falls back to
//! running whatever you typed — so known agents are two keystrokes and
//! anything else is still one command line away. Selection produces a
//! [`Message`]-style intent; the binary owns the actual spawn.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Widget;
use roster_detect::Detector;

use crate::sidebar::truncate;
use crate::style::{
    bright, muted, normal, selected as selected_surface, selected_muted, ACCENT, ACCENT_FAINT,
    ACCENT_SHINE, SURFACE_BASE, SURFACE_RAISED,
};

/// One launchable item: a display name and the command it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchItem {
    /// Display name, e.g. `claude-code`.
    pub name: String,
    /// The shell command to run.
    pub command: String,
}

/// Build the launcher's item list: every configured agent — started with
/// its `launch_command` when the config sets one (flags included), its
/// first `match_command` binary otherwise. No plain-shell row: a shell can't
/// own a workspace, so it isn't offered here (free-typed commands still run).
pub fn launch_items(detector: &Detector) -> Vec<LaunchItem> {
    detector
        .agents()
        .filter_map(|agent| {
            let command = agent
                .launch_command
                .clone()
                .or_else(|| agent.match_command.first().cloned())?;
            Some(LaunchItem {
                name: agent.name.clone(),
                command,
            })
        })
        .collect()
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

    /// Expand the selected item's command into the input for editing —
    /// add a flag, change a model — before launching it verbatim.
    pub fn expand(&mut self, items: &[LaunchItem]) {
        if let Some(index) = self.selected(items) {
            self.input = self.filtered(items)[index].command.clone();
        }
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

    /// Select the row at `index` in the filtered list (clamped by
    /// [`LauncherState::selected`] when out of range).
    pub fn select(&mut self, index: usize) {
        self.selected = index;
    }
}

/// The ASCII wordmark shown on the bare-start welcome screen: solid
/// character-fill lettering (figlet Georgia11).
const WORDMARK: [&str; 7] = [
    r#"                            mm                   "#,
    r#"                            MM                   "#,
    r#"`7Mb,od8 ,pW"Wq.  ,pP"Ybd mmMMmm .gP"Ya `7Mb,od8 "#,
    r#"  MM' "'6W'   `Wb 8I   `"   MM  ,M'   Yb  MM' "' "#,
    r#"  MM    8M     M8 `YMMMa.   MM  8M""""""  MM     "#,
    r#"  MM    YA.   ,A9 L.   I8   MM  YM.    ,  MM     "#,
    r#".JMML.   `Ybmd9'  M9mmmP'   `Mbmo`Mbmmd'.JMML.   "#,
];

/// Rows from the greeting block's top to its first item row: wordmark,
/// blank, tagline, blank, input.
const GREETING_ITEMS_OFFSET: u16 = WORDMARK.len() as u16 + 4;

/// Stand-in glyphs a flickering wordmark cell shows for a beat. None of
/// these occur in [`WORDMARK`], so tests can spot a flicker.
const FLICKER_GLYPHS: [char; 4] = ['*', '+', '~', '#'];

/// The ambient flicker: a deterministic hash decides, per wordmark cell
/// and per animation beat, whether the cell briefly shows a stand-in glyph
/// — like a lightbulb with a loose filament. Roughly one cell in 60
/// flickers on any beat; beats advance every other tick (~4/s).
fn flicker(col: usize, row: usize, tick: u64) -> Option<char> {
    let beat = tick / 2;
    let mut h = beat
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((col as u64) << 17)
        .wrapping_add((row as u64) << 41);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 29;
    h.is_multiple_of(60)
        .then(|| FLICKER_GLYPHS[((h / 60) % FLICKER_GLYPHS.len() as u64) as usize])
}
/// Rows from the modal's top to its first item row: border + input.
const MODAL_ITEMS_OFFSET: u16 = 2;

/// The launcher widget: a compact centered modal mid-session, or — with
/// [`Launcher::welcome`] — the bare-start opening screen, with the ASCII
/// wordmark over the same picker.
pub struct Launcher<'a> {
    items: &'a [LaunchItem],
    state: &'a LauncherState,
    welcome: bool,
    tick: u64,
}

impl<'a> Launcher<'a> {
    /// A launcher over `items` with the current input `state`.
    pub fn new(items: &'a [LaunchItem], state: &'a LauncherState) -> Self {
        Launcher {
            items,
            state,
            welcome: false,
            tick: u64::MAX / 2,
        }
    }

    /// Render as the bare-start welcome screen instead of the modal.
    pub fn welcome(mut self, on: bool) -> Self {
        self.welcome = on;
        self
    }

    /// The frame tick, driving the wordmark's reveal and shine.
    pub fn tick(mut self, tick: u64) -> Self {
        self.tick = tick;
        self
    }

    fn items_offset(&self) -> u16 {
        if self.welcome {
            GREETING_ITEMS_OFFSET
        } else {
            MODAL_ITEMS_OFFSET
        }
    }

    /// The centered rect the launcher occupies within `area`. The welcome
    /// block is dead-centered; the modal sits in the upper third. The
    /// minimum footprint can exceed a sliver frame, so the rect is clipped
    /// to `area` — the private `drawable` predicate is how render and
    /// hit-testing agree the result is big enough to exist.
    pub fn modal_rect(&self, area: Rect) -> Rect {
        let rows = (self.state.filtered(self.items).len() as u16).max(1);
        let (width, height) = if self.welcome {
            // wordmark + tagline + input + items + hint block
            (53u16, self.items_offset() + rows + 3)
        } else {
            // border + title + input + rows + border
            (44u16, rows + 4)
        };
        let width = width.min(area.width.saturating_sub(2)).max(20);
        let height = height.clamp(5, area.height.saturating_sub(2).max(5));
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = if self.welcome {
            area.y + (area.height.saturating_sub(height)) / 2
        } else {
            area.y + (area.height.saturating_sub(height)) / 3
        };
        Rect::new(x, y, width, height).intersection(area)
    }

    /// Where the terminal cursor belongs: the end of the typed input, on
    /// the welcome fallback strip when the full block is clipped away.
    /// `None` when no input line is drawn at all — a cursor without one
    /// is a stray blink on an empty screen.
    pub fn input_position(&self, area: Rect) -> Option<(u16, u16)> {
        let modal = self.modal_rect(area);
        let input_len = 2 + self.state.input().chars().count() as u16;
        if !drawable(modal) {
            let (x, y) = self.strip_origin(area)?;
            return Some(((x + input_len).min(area.x + area.width - 1), y));
        }
        let y = modal.y + self.items_offset() - 1;
        (y < modal.y + modal.height).then_some((
            (modal.x + 2 + input_len).min(modal.x + modal.width.saturating_sub(2)),
            y,
        ))
    }

    /// Where the welcome fallback strip's prompt goes: one column in, on
    /// the middle row. `Some` only on the welcome screen when a strip
    /// fits at all; callers gate on the full block being undrawable.
    /// Render and the cursor derive from this one spot so they can't
    /// disagree.
    fn strip_origin(&self, area: Rect) -> Option<(u16, u16)> {
        (self.welcome && area.width >= 4 && area.height > 0)
            .then_some((area.x + 1, area.y + area.height / 2))
    }

    /// The one-line welcome fallback: prompt, typed input, and what enter
    /// launches — a bare start in a sliver terminal stays legible instead
    /// of blank. Only rendered when the full block cannot draw.
    fn render_strip(&self, area: Rect, buf: &mut Buffer) {
        let Some((x, y)) = self.strip_origin(area) else {
            return;
        };
        buf.set_stringn(
            x,
            y,
            "❯",
            usize::from(area.x + area.width - x),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        );
        let text_x = x + 2;
        let mut budget = usize::from(area.x + area.width - text_x);
        // What enter launches, right-aligned and quiet, when it fits
        // beside the input.
        if let Some(index) = self.state.selected(self.items) {
            let label = format!("↵ {}", self.state.filtered(self.items)[index].name);
            let len = label.chars().count() as u16;
            let input_end = text_x + self.state.input().chars().count() as u16;
            let label_x = (area.x + area.width).saturating_sub(len + 1);
            if label_x > input_end + 2 {
                buf.set_string(label_x, y, &label, muted());
                budget = usize::from(label_x - 1 - text_x);
            }
        }
        if self.state.input().is_empty() {
            // Whole or not at all: a chopped placeholder ("❯ ty") reads
            // as garbage, and unlike typed input it carries no state.
            let hint = "type a command…";
            if budget >= hint.chars().count() {
                buf.set_stringn(text_x, y, hint, budget, muted());
            }
        } else {
            buf.set_stringn(
                text_x,
                y,
                self.state.input(),
                budget,
                bright().add_modifier(Modifier::BOLD),
            );
        }
    }

    /// Whether (`x`, `y`) falls inside the launcher block. Always false
    /// when the launcher is too small to draw.
    pub fn contains(&self, area: Rect, x: u16, y: u16) -> bool {
        let modal = self.modal_rect(area);
        drawable(modal)
            && x >= modal.x
            && x < modal.x + modal.width
            && y >= modal.y
            && y < modal.y + modal.height
    }

    /// One past the last row that may hold an item: the welcome block is
    /// borderless, the modal keeps its bottom border row. Render stops
    /// drawing here and [`Launcher::item_at`] refuses clicks past it — a
    /// height-clamped list must not launch from rows it never drew.
    fn rows_bottom(&self, modal: Rect) -> u16 {
        if self.welcome {
            modal.y + modal.height
        } else {
            modal.y + modal.height - 1
        }
    }

    /// The filtered item row under (`x`, `y`), when one is there.
    pub fn item_at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let modal = self.modal_rect(area);
        if !self.contains(area, x, y)
            || y < modal.y + self.items_offset()
            || y >= self.rows_bottom(modal)
        {
            return None;
        }
        let index = usize::from(y - modal.y - self.items_offset());
        let _ = x;
        (index < self.state.filtered(self.items).len()).then_some(index)
    }
}

/// Whether a clipped modal rect is big enough to draw at all. Render bails
/// when this is false, and the hit tests consult it too — a modal that
/// isn't on screen must never own a click.
fn drawable(modal: Rect) -> bool {
    modal.width >= 8 && modal.height >= 5
}

impl Widget for Launcher<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let modal = self.modal_rect(area);
        if !drawable(modal) {
            // The welcome screen is the whole UI at bare start — a frame
            // too small for the block gets a one-line strip instead of
            // going blank while keystrokes still drive the picker. The
            // mid-session modal just vanishes: panes stay visible and
            // esc is the way out.
            self.render_strip(area, buf);
            return;
        }
        // Everything below writes only inside the clipped modal: rows past
        // `bottom` are skipped, not drawn — set_stringn past the buffer
        // panics, and the welcome layout's fixed offsets (wordmark, then
        // tagline, input, hints) don't fit short frames.
        let bottom = modal.y + modal.height;
        // The modal is a dialog on the raised surface; the welcome block
        // sits directly on the app canvas.
        let bg = if self.welcome {
            SURFACE_BASE
        } else {
            SURFACE_RAISED
        };
        fill(buf, modal, bg);
        let mut y = modal.y;
        if self.welcome {
            // The opening screen: the wordmark sweeps in left to right,
            // then a band of shine drifts across it on a slow loop.
            let mark_width = WORDMARK
                .iter()
                .map(|row| row.trim_end().chars().count())
                .max()
                .unwrap_or(0) as u16;
            let mark_x = modal.x + (modal.width.saturating_sub(mark_width)) / 2;
            let revealed = self.tick.saturating_mul(6).min(u64::from(mark_width)) as u16;
            let shine_cycle = u64::from(mark_width) + 32;
            let shine = ((self.tick * 2) % shine_cycle) as i32 - 16;
            for (row, text) in WORDMARK.iter().enumerate() {
                for (col, ch) in text.chars().enumerate() {
                    if col as u16 >= revealed || ch == ' ' {
                        continue;
                    }
                    let mut glyph = ch;
                    let mut style = Style::default().fg(ACCENT);
                    if revealed >= mark_width && (0..6).contains(&(col as i32 - shine)) {
                        // A pale tint of the accent, so the sweep reads as
                        // light glancing off the wordmark rather than a
                        // hard cold-white flash.
                        style = Style::default()
                            .fg(ACCENT_SHINE)
                            .add_modifier(Modifier::BOLD);
                    }
                    if let Some(stand_in) = flicker(col, row, self.tick) {
                        glyph = stand_in;
                        // An explicit deep tint, never DIM — the faint
                        // attribute is reserved for guest cells and
                        // vanishes on many default palettes.
                        style = Style::default().fg(ACCENT_FAINT);
                    }
                    let (x, y) = (mark_x + col as u16, modal.y + row as u16);
                    if y >= bottom {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(glyph);
                        cell.set_style(style);
                    }
                }
            }
            y += WORDMARK.len() as u16 + 1;
            let tagline = "terminal multiplexer for Claude Code";
            let tag_x = modal.x + (modal.width.saturating_sub(tagline.chars().count() as u16)) / 2;
            if y < bottom {
                buf.set_stringn(tag_x, y, tagline, usize::from(modal.width), muted());
            }
            y += 2;
        } else {
            frame(buf, modal, " new agent ");
            y += 1;
        }

        let inner_x = modal.x + 2;
        let inner_w = usize::from(modal.width.saturating_sub(4));

        // Input line, prompt-style: the accent prompt marks where typing
        // lands, the typed text takes the bright tier.
        if y < bottom {
            buf.set_stringn(
                inner_x,
                y,
                "❯",
                inner_w,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            );
            buf.set_stringn(
                inner_x + 2,
                y,
                self.state.input(),
                inner_w.saturating_sub(2),
                bright().add_modifier(Modifier::BOLD),
            );
        }
        y += 1;

        let filtered = self.state.filtered(self.items);
        let selected = self.state.selected(self.items);
        let items_bottom = modal.y + self.items_offset() + (filtered.len() as u16).max(1);
        if filtered.is_empty() && y < bottom {
            let typed = self.state.input().trim().to_string();
            let hint = if typed.is_empty() {
                "type a command…".to_string()
            } else {
                format!("↵ run: {typed}")
            };
            buf.set_stringn(inner_x, y, hint, inner_w, muted());
        }
        let rows_bottom = self.rows_bottom(modal);
        for (index, item) in filtered.iter().enumerate() {
            if y >= rows_bottom {
                break;
            }
            // The selected row is the selected surface — the same light
            // fill that marks the focused sidebar card. Painting fg AND bg
            // (never a REVERSED overlay) keeps the bar one continuous
            // color regardless of what each cell's foreground was.
            let is_selected = selected == Some(index);
            if is_selected {
                buf.set_style(
                    Rect::new(modal.x + 1, y, modal.width - 2, 1),
                    selected_surface(),
                );
            }
            let marker = if is_selected { "❯" } else { " " };
            buf.set_stringn(
                inner_x,
                y,
                format!("{marker} {}", item.name),
                inner_w,
                if is_selected {
                    selected_surface().add_modifier(Modifier::BOLD)
                } else {
                    normal()
                },
            );
            // The command sits right-aligned and quiet — truncated with an
            // ellipsis when it would otherwise run into the item's name.
            // Width math is in display cells (user config can hold wide
            // chars): counted in chars, a wide command both starts too far
            // right and paints through the modal's right border.
            let cmd_style = if is_selected {
                selected_muted()
            } else {
                muted()
            };
            // usize throughout: a config string past 65k cells would wrap
            // u16 arithmetic; the casts back down are bounded by the modal.
            let name_end = usize::from(inner_x) + 2 + Span::raw(item.name.as_str()).width();
            let right_edge = usize::from(modal.x + modal.width - 2);
            let avail = right_edge.saturating_sub(name_end + 2);
            let cmd_cells = Span::raw(item.command.as_str()).width();
            if cmd_cells <= avail {
                buf.set_string((right_edge - cmd_cells) as u16, y, &item.command, cmd_style);
            } else if avail >= 8 {
                let cut = truncate(&item.command, avail);
                let cut_cells = Span::raw(cut.as_str()).width();
                buf.set_string((right_edge - cut_cells) as u16, y, &cut, cmd_style);
            }
            y += 1;
        }

        if self.welcome {
            // Claude Code is the agent; anything else runs as a plain support
            // pane (a shell, a dev server) with no state detection.
            let hints = [
                "…or run a command — enter runs it",
                "add your own cards: roster --print-config",
            ];
            for (row, hint) in hints.iter().enumerate() {
                let hint_y = items_bottom + 1 + row as u16;
                if hint_y < bottom {
                    buf.set_stringn(inner_x, hint_y, *hint, inner_w, muted());
                }
            }
        }
    }
}

/// Blank the modal's cells onto `bg` so panes underneath don't bleed
/// through and the block reads as a surface, not a hole: dialogs sit on
/// the raised surface, the welcome block on the base canvas.
pub(crate) fn fill(buf: &mut Buffer, rect: Rect, bg: Color) {
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(Style::default().bg(bg));
            }
        }
    }
}

/// Draw a rounded border with a title, in the accent color.
pub(crate) fn frame(buf: &mut Buffer, rect: Rect, title: &str) {
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
        launch_items(&Detector::builtin())
    }

    #[test]
    fn launch_items_lists_agents_only() {
        let items = items();
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["claude-code"], "no plain-shell row");
        assert_eq!(items[0].command, "claude");
    }

    #[test]
    fn launch_command_overrides_the_bare_binary() {
        let detector = Detector::from_toml(
            r#"
            [claude-code]
            match_command = ["claude"]
            launch_command = "claude --dangerously-skip-permissions"

            [worker]
            match_command = ["worker"]
            "#,
        )
        .unwrap();
        let items = launch_items(&detector);
        assert_eq!(items[0].name, "claude-code");
        assert_eq!(items[0].command, "claude --dangerously-skip-permissions");
        assert_eq!(items[1].command, "worker", "no override, bare binary");
    }

    #[test]
    fn expand_pulls_the_selected_command_into_the_input() {
        let items = items();
        let mut state = LauncherState::new();
        for c in "cla".chars() {
            state.push(c);
        }
        state.expand(&items);
        assert_eq!(state.input(), "claude");
        // Editing after expansion runs the edited text verbatim.
        for c in " --continue".chars() {
            state.push(c);
        }
        assert_eq!(state.command(&items).as_deref(), Some("claude --continue"));
        // Expanding with no match is a no-op.
        let mut none = LauncherState::new();
        for c in "zzz".chars() {
            none.push(c);
        }
        none.expand(&items);
        assert_eq!(none.input(), "zzz");
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
        assert_eq!(state.command(&items).as_deref(), Some("claude"));
    }

    #[test]
    fn wide_char_commands_stay_inside_the_modal() {
        // User config can put wide chars in a launch command; counted in
        // chars the right-aligned column starts too far right and paints
        // through the modal's right border.
        let items = vec![LaunchItem {
            name: "claude-code".into(),
            command: "claude 二十文字のコマンド".into(),
        }];
        let state = LauncherState::new();
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state).render(area, &mut buf);
        let row_y = (0..20u16)
            .find(|y| (0..60u16).any(|x| buf.cell((x, *y)).unwrap().symbol() == "二"))
            .expect("command row rendered");
        let row: Vec<String> = (0..60u16)
            .map(|x| buf.cell((x, row_y)).unwrap().symbol().to_string())
            .collect();
        let last = row.iter().rposition(|s| !s.trim().is_empty()).unwrap();
        assert_eq!(
            row[last],
            "│",
            "command painted through the modal border: {}",
            row.concat()
        );
    }

    #[test]
    fn selection_wraps_and_backspace_refilters() {
        let detector = Detector::from_toml(
            r#"
            [claude-code]
            match_command = ["claude"]

            [worker]
            match_command = ["worker"]
            "#,
        )
        .unwrap();
        let items = launch_items(&detector);
        let mut state = LauncherState::new();
        state.select_prev(&items);
        assert_eq!(state.selected(&items), Some(1));
        state.select_next(&items);
        assert_eq!(state.selected(&items), Some(0));

        state.push('z');
        state.push('z');
        assert!(state.filtered(&items).is_empty());
        assert_eq!(state.command(&items).as_deref(), Some("zz"));
        state.backspace();
        state.backspace();
        assert_eq!(state.filtered(&items).len(), 2);
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
    fn modal_sits_on_the_raised_surface_and_welcome_on_the_canvas() {
        let items = items();
        let state = LauncherState::new();
        let area = Rect::new(0, 0, 80, 24);
        // The mid-session modal is a dialog: raised fill edge to edge.
        let launcher = Launcher::new(&items, &state);
        let modal = launcher.modal_rect(area);
        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state).render(area, &mut buf);
        for (x, y) in [
            (modal.x, modal.y),
            (modal.x + 2, modal.y + 1),
            (modal.x + modal.width - 1, modal.y + modal.height - 1),
        ] {
            assert_eq!(
                buf.cell((x, y)).unwrap().style().bg,
                Some(SURFACE_RAISED),
                "modal cell ({x},{y})"
            );
        }
        // The selected row (the first item, at the items offset) is the
        // selected surface — one continuous light fill, name and command
        // both dark on it.
        let row_y = modal.y + MODAL_ITEMS_OFFSET;
        for x in [modal.x + 1, modal.x + 4, modal.x + modal.width - 2] {
            assert_eq!(
                buf.cell((x, row_y)).unwrap().style().bg,
                selected_surface().bg,
                "selected row cell ({x},{row_y})"
            );
        }
        assert_eq!(
            buf.cell((modal.x + 4, row_y)).unwrap().style().fg,
            selected_surface().fg
        );
        // A cell outside the modal keeps whatever was under it.
        assert_ne!(buf.cell((0, 0)).unwrap().style().bg, Some(SURFACE_RAISED));
        // The welcome block is the opening screen, not a dialog: it sits
        // directly on the base canvas.
        let welcome = Launcher::new(&items, &state).welcome(true);
        let block = welcome.modal_rect(area);
        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state)
            .welcome(true)
            .tick(64)
            .render(area, &mut buf);
        assert_eq!(
            buf.cell((block.x + 1, block.y + 1)).unwrap().style().bg,
            Some(SURFACE_BASE)
        );
    }

    #[test]
    fn wordmark_flicker_and_shine_are_explicit_tints_never_dim() {
        let items = items();
        let state = LauncherState::new();
        let area = Rect::new(0, 0, 80, 24);
        let block = Launcher::new(&items, &state).welcome(true).modal_rect(area);
        // The wordmark owns the block's first rows; item names, commands,
        // and hints below may legitimately contain the stand-in glyphs.
        let mark_rows = block.y..block.y + WORDMARK.len() as u16;
        let (mut saw_flicker, mut saw_shine) = (false, false);
        for tick in 0..80u64 {
            let mut buf = Buffer::empty(area);
            Launcher::new(&items, &state)
                .welcome(true)
                .tick(tick)
                .render(area, &mut buf);
            for y in 0..24u16 {
                for x in 0..80u16 {
                    let cell = buf.cell((x, y)).unwrap();
                    // The regression this extends (see style.rs): roster
                    // chrome never leans on the DIM attribute — the
                    // flicker's faint is an explicit deep accent tint.
                    assert!(
                        !cell.style().add_modifier.contains(Modifier::DIM),
                        "DIM at ({x},{y}), tick {tick}"
                    );
                    let glyph = cell.symbol().chars().next().unwrap_or(' ');
                    if mark_rows.contains(&y) && FLICKER_GLYPHS.contains(&glyph) {
                        assert_eq!(cell.style().fg, Some(ACCENT_FAINT));
                        saw_flicker = true;
                    }
                    if cell.style().fg == Some(ACCENT_SHINE) {
                        saw_shine = true;
                    }
                }
            }
        }
        assert!(saw_flicker, "no flicker cell rendered in 80 ticks");
        assert!(saw_shine, "no shine cell rendered in 80 ticks");
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
    }

    #[test]
    fn sliver_frames_neither_draw_nor_hit() {
        // Frames too small for the modal's minimum footprint: the
        // mid-session modal draws nothing at all (buffer-equality catches
        // style-only writes too), the welcome screen degrades to its
        // one-line strip, and no click lands on the invisible modal
        // either way.
        let items = items();
        let state = LauncherState::new();
        for welcome in [false, true] {
            for (w, h) in [(1u16, 1u16), (7, 24), (80, 3), (80, 4)] {
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::empty(area);
                Launcher::new(&items, &state)
                    .welcome(welcome)
                    .render(area, &mut buf);
                let launcher = Launcher::new(&items, &state).welcome(welcome);
                if welcome && w >= 4 {
                    let strip: String = (0..w)
                        .map(|x| buf.cell((x, h / 2)).unwrap().symbol().to_string())
                        .collect();
                    assert!(strip.contains('❯'), "no strip prompt at {w}x{h}: {strip}");
                    assert!(launcher.input_position(area).is_some(), "{w}x{h}");
                } else {
                    assert_eq!(
                        buf,
                        Buffer::empty(area),
                        "drawn at {w}x{h} welcome={welcome}"
                    );
                    assert_eq!(launcher.input_position(area), None, "{w}x{h}");
                }
                for y in 0..h {
                    for x in 0..w {
                        assert!(
                            !launcher.contains(area, x, y),
                            "phantom contains at ({x},{y}) in {w}x{h} welcome={welcome}"
                        );
                        assert_eq!(
                            launcher.item_at(area, x, y),
                            None,
                            "phantom item at ({x},{y}) in {w}x{h} welcome={welcome}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn welcome_sliver_shows_prompt_input_and_selection() {
        // The fallback strip is legible, not just non-blank: the prompt,
        // a placeholder (then the typed text), and what enter launches.
        let items = items();
        let mut state = LauncherState::new();
        let area = Rect::new(0, 0, 40, 3);
        let row = |buf: &Buffer, y: u16| -> String {
            (0..40u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        };

        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state)
            .welcome(true)
            .render(area, &mut buf);
        let strip = row(&buf, 1);
        assert!(strip.contains("❯ type a command…"), "strip: {strip}");
        assert!(strip.contains("↵ claude-code"), "strip: {strip}");

        state.push('c');
        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state)
            .welcome(true)
            .render(area, &mut buf);
        let strip = row(&buf, 1);
        assert!(strip.contains("❯ c"), "typed input missing: {strip}");
        assert!(!strip.contains("type a command…"), "stale hint: {strip}");
        // Cursor sits after the typed character on the strip row.
        let launcher = Launcher::new(&items, &state).welcome(true);
        assert_eq!(launcher.input_position(area), Some((4, 1)));
    }

    #[test]
    fn clipped_frames_draw_what_fits_without_panicking() {
        // The drawable-but-cramped band: tall enough to pass the floor,
        // too short for the full layout (the welcome block wants ~15
        // rows). Writes past the clipped modal are skipped, not drawn —
        // on main this whole band panicked with an out-of-bounds index.
        let items = items();
        let state = LauncherState::new();
        for welcome in [false, true] {
            for h in 5..=16u16 {
                let area = Rect::new(0, 0, 80, h);
                let mut buf = Buffer::empty(area);
                Launcher::new(&items, &state)
                    .welcome(welcome)
                    .tick(99)
                    .render(area, &mut buf);
            }
        }
    }

    #[test]
    fn height_clamped_list_refuses_clicks_on_undrawn_rows() {
        // More items than the clamped modal can show: rows render only
        // above the bottom border, and item_at must agree — clicking the
        // border (or below) must not launch something never drawn.
        let items: Vec<LaunchItem> = (0..20)
            .map(|i| LaunchItem {
                name: format!("agent-{i}"),
                command: format!("cmd-{i}"),
            })
            .collect();
        let state = LauncherState::new();
        let area = Rect::new(0, 0, 80, 12);
        let launcher = Launcher::new(&items, &state);
        let modal = launcher.modal_rect(area);
        let mut buf = Buffer::empty(area);
        Launcher::new(&items, &state).render(area, &mut buf);

        let border_row = modal.y + modal.height - 1;
        let last_item_row = border_row - 1;
        assert!(
            launcher.item_at(area, modal.x + 2, last_item_row).is_some(),
            "last drawn row should still hit"
        );
        assert_eq!(
            launcher.item_at(area, modal.x + 2, border_row),
            None,
            "border row hit an undrawn item"
        );
    }
}
