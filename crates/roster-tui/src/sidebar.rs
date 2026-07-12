//! The agent-state sidebar: every agent across every workspace in one flat
//! list, each rendered as a card — a colored status glyph, the agent name,
//! and its age on top; the reason (why the agent is in its state) below,
//! with the ` auto ` auto-approve pill revealed on the cards you're on;
//! and a third line of telemetry badges only where they earn the row —
//! the full line on the focused card, the escalated context badge anywhere
//! (see `telemetry.rs`). The glyph alone carries the state: its shape and
//! color are distinct per state, so the detail row spends its width on the
//! reason — the signal — not on spelling the state out twice. Cards are
//! ranked globally by `roster_core::attention` — blocked, done, idle, then
//! working at the bottom — so the agents that need you are always at the
//! top, regardless of which workspace they live in. With more than one
//! workspace each card carries a `⧉N` tag naming its home. Cards sit as
//! raised surfaces on the panel's base canvas; the card whose pane holds
//! focus is the one *inverted* card — light fill, dark text, plus the
//! accent bar down its left edge — so the sidebar always answers "which
//! agent am I looking at" from across the room. When any pane's feed
//! reports the account's rate limits, a footer pinned to the panel's
//! bottom shows each window as a labeled usage bar
//! (see [`limits_footer_height`]); without one the sidebar renders exactly
//! its footer-less self.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use roster_core::{
    AgentState, AttentionItem, PaneId, RateLimit, RateLimitWindow, Session, Telemetry,
};
use roster_detect::Detector;

use crate::style::{
    bright, chip, muted, normal, selected, selected_muted, state_color, state_glyph,
    state_glyph_style, state_glyph_style_selected, state_label, SURFACE_BASE, SURFACE_RAISED,
};
use crate::telemetry::{context_badge, limit_style, telemetry_line, telemetry_row_visible};

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
    /// The pane's live terminal title — the agent's current task, when it
    /// broadcasts one. The card's first line prefers it over the config
    /// name: in a Claude-only sidebar every card saying `claude-code`
    /// carries no information, but the task does.
    pub title: Option<String>,
    /// The agent's own name for its session, from the statusline feed —
    /// the name fallback when no title was ever broadcast (a session whose
    /// first prompt is a slash command never gets a title summary).
    pub session_name: Option<String>,
}

impl SidebarEntry {
    /// The name the chrome shows for this pane: the live task title when
    /// the agent broadcast a non-blank one, else the agent's own session
    /// name when the statusline feed reported one, else the agent's config
    /// name. The one resolver shared by the sidebar card and the pane
    /// border, so the two surfaces can't disagree about what a pane is
    /// called — and the blank-text guard lives here rather than at each
    /// render site.
    pub fn display_name(&self) -> &str {
        [self.title.as_deref(), self.session_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|name| !name.is_empty())
            .unwrap_or(&self.agent)
    }
}

/// Build the sidebar rows from the session: every pane whose command
/// identifies as a configured agent, ranked globally by
/// `roster_core::attention` — blocked first (longest wait leading), then
/// done, idle, and working at the bottom — across all workspaces at once,
/// so the most blocked agent anywhere rises to the top.
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
                title: pane.title.clone(),
                session_name: pane.session_name.clone(),
            })
        })
        .collect();
    entries.sort_by_key(|e| {
        // Committed state and age only, so the order can't jitter as raw
        // readings bounce. `destructive` has no session-model source yet
        // (the hook bridge will feed it — docs/05); false ranks the ask as
        // plain. The pane id breaks exact ties, keeping equal-priority
        // cards in a stable position frame to frame. No workspace term: the
        // ranking is global, so the most blocked agent anywhere leads.
        let item = AttentionItem {
            state: e.state,
            waiting_for: e.age,
            destructive: false,
        };
        (item.priority(), e.pane)
    });
    entries
}

/// The entries index of the pane holding focus, when it has a card. The
/// one resolver shared by render and hit-testing: the focused card is the
/// card that grows the full telemetry row, so the two sides resolving
/// focus differently would desync the row plan and land every click below
/// the focused card one row off.
pub fn focused_entry(entries: &[SidebarEntry], focused: Option<PaneId>) -> Option<usize> {
    focused.and_then(|id| entries.iter().position(|entry| entry.pane == id))
}

/// A pane-switch request surfaced by the sidebar. The binary owns the
/// side effect; this crate only ever emits the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    /// Focus the given pane.
    JumpToPane(PaneId),
}

/// Keyboard-navigation state for the sidebar: which pane is selected.
///
/// Selection is identity-based, not positional: `sidebar_entries` re-triages
/// rows by attention priority every frame, so a pane whose committed state
/// flips between keystrokes changes rows. Anchoring to the pane keeps the
/// highlight — and anything acted on it (jump, auto-approve) — on the agent
/// the user chose. The remembered row is only the fallback for when the
/// anchored pane disappears from the entries.
#[derive(Clone, Copy, Debug, Default)]
pub struct SidebarState {
    pane: Option<PaneId>,
    index: usize,
}

impl SidebarState {
    /// A state with the first row selected and no pane anchored yet.
    pub fn new() -> Self {
        SidebarState::default()
    }

    /// A state anchored to the top row's pane — the selection jump mode
    /// opens with. Anchoring at construction keeps the unanchored-but-
    /// selectable state out of the key handlers: a re-triage before the
    /// first keystroke can't hand the selection to whichever pane rises
    /// to row zero.
    pub fn anchored(entries: &[SidebarEntry]) -> Self {
        let mut state = SidebarState::default();
        state.anchor(0, entries);
        state
    }

    /// The selected row: the anchored pane's current position in `entries`,
    /// or the last-anchored row (clamped) when the pane is gone or none was
    /// anchored. `None` when there are no rows.
    pub fn selected(&self, entries: &[SidebarEntry]) -> Option<usize> {
        if entries.is_empty() {
            return None;
        }
        let anchored = self
            .pane
            .and_then(|pane| entries.iter().position(|entry| entry.pane == pane));
        Some(anchored.unwrap_or(self.index.min(entries.len() - 1)))
    }

    /// Move the selection down one row, wrapping.
    pub fn select_next(&mut self, entries: &[SidebarEntry]) {
        if let Some(current) = self.selected(entries) {
            self.anchor((current + 1) % entries.len(), entries);
        }
    }

    /// Move the selection up one row, wrapping.
    pub fn select_prev(&mut self, entries: &[SidebarEntry]) {
        if let Some(current) = self.selected(entries) {
            self.anchor((current + entries.len() - 1) % entries.len(), entries);
        }
    }

    /// The intent behind pressing enter: jump to the selected entry's pane.
    pub fn activate(&self, entries: &[SidebarEntry]) -> Option<Message> {
        let index = self.selected(entries)?;
        Some(Message::JumpToPane(entries[index].pane))
    }

    fn anchor(&mut self, index: usize, entries: &[SidebarEntry]) {
        if let Some(entry) = entries.get(index) {
            self.pane = Some(entry.pane);
            self.index = index;
        }
    }
}

/// One row of the sidebar's card region, in order from the top. Render
/// draws this plan and hit-testing mirrors it, so the two can't drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    /// The first line of an entry's card: glyph, name, age.
    EntryName(usize),
    /// The second line of an entry's card: the reason, and the `auto` chip
    /// when revealed.
    EntryDetail(usize),
    /// The third line of an entry's card: telemetry badges. Emitted only
    /// where the row earns its height (see `telemetry_row_visible`) — the
    /// focused card, or a card whose context alert escalated — so at-rest
    /// cards keep their two-line shape.
    EntryTelemetry(usize),
    /// Breathing room.
    Blank,
}

/// The sidebar's card-region rows for `entries`: one flat, globally-ranked
/// list of cards with no workspace headers — the ranking already interleaves
/// workspaces, so there is nothing to group under. `focused` is the entry
/// whose pane holds focus; only its card grows the full telemetry row.
/// Render draws this plan and hit-testing mirrors it, so both must resolve
/// `focused` the same way.
pub fn sidebar_rows(entries: &[SidebarEntry], focused: Option<usize>) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        rows.push(SidebarRow::EntryName(index));
        rows.push(SidebarRow::EntryDetail(index));
        if entry
            .telemetry
            .as_ref()
            .is_some_and(|telemetry| telemetry_row_visible(telemetry, focused == Some(index)))
        {
            rows.push(SidebarRow::EntryTelemetry(index));
        }
        rows.push(SidebarRow::Blank);
    }
    rows
}

/// The `auto` chip's text — the per-card auto-approve toggle, space-padded
/// into the reverse-video pill `style::chip` draws. The pill is the
/// affordance: it reads as pressable even in terminals that ignore the
/// pointer-shape protocol and show no hand cursor.
const AUTO_CHIP: &str = " auto ";

/// The minimum reason budget the chip must leave on its row. The reason is
/// the signal and outranks chrome: a sidebar too cramped for both drops
/// the chip (the keyboard toggle still works), never the reason.
const MIN_REASON: u16 = 8;

/// The card body's left indent, in sidebar-inner columns.
const CARD_INDENT: u16 = 4;

/// The columns of an entry detail row's `auto` chip, in sidebar-inner
/// columns: the pill right-aligned one column in from the edge, mirroring
/// the age on the name row above. `None` when the row can't host it and a
/// useful reason. Width-only, and the reason's budget always stops short
/// of these columns even while the chip is hidden — an unarmed chip is
/// revealed by hover, and text must not reflow under the pointer. Render
/// draws it and `hit_test` targets it, so the chip can't drift off its
/// click target.
pub fn auto_chip_cols(width: u16) -> Option<std::ops::Range<u16>> {
    let chip = AUTO_CHIP.chars().count() as u16;
    // Card indent + the reason's reserve + the gutter before the chip.
    let gutter = 1;
    let taken = CARD_INDENT + MIN_REASON + gutter;
    (width > taken + chip).then(|| width - 1 - chip..width - 1)
}

/// The fleet toggle's text — arms auto-approve for every agent at once.
/// Space-padded into the same pill as the per-card chip.
const AUTO_ALL: &str = " auto-yes ";

/// The columns of the sidebar header's `auto-yes` fleet toggle, in
/// sidebar-inner columns: right-aligned one column in from the edge,
/// mirroring the per-card chip below it. `None` on sidebars too narrow to
/// keep it clear of the inline blocked count on the left. Width-only and
/// static, so the button never jumps as the blocked count comes and goes.
/// Render draws it and `hit_test` targets it, so the button can't drift
/// off its click target.
pub fn auto_all_cols(width: u16) -> Option<std::ops::Range<u16>> {
    let button = AUTO_ALL.chars().count() as u16;
    // " agents" label + gap + the widest plausible count + gutter.
    let label = 1 + 6; // leading pad + "agents"
    let gap = 2;
    let widest_count = 9; // "9 blocked"
    let gutter = 1;
    let taken = label + gap + widest_count + gutter;
    (width > taken + button).then(|| width - 1 - button..width - 1)
}

/// Cells of a footer window's usage bar. Fixed rather than width-scaled:
/// the bar is a gauge read at a glance, and a length that changes with the
/// sidebar would make the same percentage look different across layouts.
/// Six, not more — the bar only ranks the percentage beside it, and every
/// cell it takes comes out of the reset tail's budget on the default
/// sidebar width.
const LIMIT_BAR_WIDTH: usize = 6;

/// The rows the sidebar's fleet rate-limit footer occupies at the bottom
/// of a card region `height` rows tall: one row per reported window plus a
/// blank spacer above, or 0 with nothing to show. The footer yields whole
/// on a sidebar too short to keep the header and at least one card above
/// it — the cards are the product, the footer is chrome. Render draws this
/// plan and `hit_test` subtracts it, so a click on the footer can't land
/// on a card the shrunken region no longer shows.
pub fn limits_footer_height(limits: Option<&RateLimit>, height: u16) -> u16 {
    let lines = limits.map_or(0, |limits| {
        u16::from(limits.five_hour.is_some()) + u16::from(limits.seven_day.is_some())
    });
    if lines == 0 {
        return 0;
    }
    // Header + its blank + a two-row card + its blank must survive above.
    let total = lines + 1;
    if height < total + 5 {
        return 0;
    }
    total
}

/// One footer line: label, usage bar, percentage, and the reset time when
/// the window carries one. The filled cells and the percentage wear the
/// window's severity (see `limit_style`); the empty cells and the reset
/// stay quiet chrome, so the escalation reads from the numbers that mean
/// it.
fn limit_line(label: &str, window: &RateLimitWindow) -> Line<'static> {
    let severity = limit_style(window.used_pct);
    // A NaN share casts to 0 filled cells — the bar under-promises on
    // garbage rather than painting a full gauge.
    let filled = (window.used_pct / 100.0 * LIMIT_BAR_WIDTH as f32)
        .round()
        .clamp(0.0, LIMIT_BAR_WIDTH as f32) as usize;
    // Percent right-aligned to three cells so the two rows column-align
    // and the reset tails start together; the worst line ("100%", a "23h"
    // reset) is 27 cells against the default sidebar's 29-cell budget.
    let mut spans = vec![
        Span::styled(format!("{label} "), muted()),
        Span::styled("▓".repeat(filled), severity),
        Span::styled("░".repeat(LIMIT_BAR_WIDTH - filled), muted()),
        // Floored, not rounded: the color comes from the raw share, so a
        // rounded 89.6 would read "90%" in the warn yellow — the number
        // must never name a tier its color doesn't wear.
        Span::styled(format!(" {:>3.0}%", window.used_pct.floor()), severity),
    ];
    if let Some(resets) = window.resets_in {
        spans.push(Span::styled(
            format!(" · resets {}", format_age(resets)),
            muted(),
        ));
    }
    Line::from(spans)
}

/// The agent-state sidebar widget.
pub struct Sidebar<'a> {
    entries: &'a [SidebarEntry],
    selected: Option<usize>,
    hovered: Option<usize>,
    hovered_auto: Option<usize>,
    hovered_auto_all: bool,
    focused: Option<usize>,
    workspaces: usize,
    tick: u64,
    rate_limits: Option<&'a RateLimit>,
}

impl<'a> Sidebar<'a> {
    /// A sidebar over `entries`, highlighting `selected` when given and
    /// giving `hovered` (the card under the mouse) a dim marker.
    /// `workspaces` is the session's window count; with more than one, each
    /// card carries a `⧉N` tag naming its home. `tick` animates the working
    /// spinner.
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
            hovered_auto_all: false,
            focused: None,
            workspaces,
            tick,
            rate_limits: None,
        }
    }

    /// The account's fleet-aggregated rate-limit reading, shown as a footer
    /// pinned to the panel's bottom. `None` — no pane has live rate-limit
    /// telemetry — renders exactly the footer-less sidebar from before the
    /// field existed.
    pub fn rate_limits(mut self, limits: Option<&'a RateLimit>) -> Self {
        self.rate_limits = limits;
        self
    }

    /// The entry index whose pane holds focus, marked with an accent bar
    /// down the card's left edge — the sidebar's "you are here". Distinct
    /// from `selected`, which is the transient jump-mode highlight.
    pub fn focused(mut self, index: Option<usize>) -> Self {
        self.focused = index;
        self
    }

    /// The entry index whose `auto` chip is under the mouse, for hover
    /// highlighting.
    pub fn hovered_auto(mut self, index: Option<usize>) -> Self {
        self.hovered_auto = index;
        self
    }

    /// Whether the header's `auto-yes` fleet toggle is under the mouse.
    pub fn hovered_auto_all(mut self, hovered: bool) -> Self {
        self.hovered_auto_all = hovered;
        self
    }

    /// Draw one cell of the focused card's accent edge bar. One helper for
    /// the three card rows, so the bar's glyph and color can't drift apart
    /// between them.
    fn draw_focus_bar(buf: &mut Buffer, x: u16, y: u16) {
        buf.set_string(x, y, "▍", Style::default().fg(crate::style::ACCENT));
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
        // The panel canvas: every cell sits on the base surface, so the
        // raised cards have something to stand on. (The frame renderer
        // fills the whole sidebar region too — this keeps the widget
        // self-contained for direct rendering in tests.)
        buf.set_style(area, Style::default().bg(SURFACE_BASE));
        let width = usize::from(area.width);
        // The fleet rate-limit footer claims the bottom rows first; cards
        // stop above it. Drawn on the base canvas — account-scoped data
        // belongs to the panel, not to any one card's raised surface.
        let footer = limits_footer_height(self.rate_limits, area.height);
        let bottom = area.y + area.height - footer;
        if footer > 0 {
            let five = self
                .rate_limits
                .and_then(|limits| limits.five_hour.as_ref());
            let seven = self
                .rate_limits
                .and_then(|limits| limits.seven_day.as_ref());
            // Indent + right margin, like the header row above.
            let budget = width.saturating_sub(2);
            let mut line_y = bottom + 1;
            for (label, window) in [("5h", five), ("wk", seven)] {
                let Some(window) = window else { continue };
                let line = truncate_line(limit_line(label, window), budget, muted());
                buf.set_line(area.x + 1, line_y, &line, area.width.saturating_sub(1));
                line_y += 1;
            }
        }
        let mut y = area.y;

        // Quiet header: lowercase, dim; the blocked count follows it inline
        // (only when someone actually needs you), and the `auto-yes`
        // fleet toggle holds the right edge at fixed columns.
        let blocked = self.blocked_count();
        buf.set_stringn(area.x + 1, y, "agents", width.saturating_sub(1), muted());
        if blocked > 0 {
            let summary = format!("{blocked} blocked");
            let budget = auto_all_cols(area.width)
                .map(|cols| cols.start.saturating_sub(1 + 6 + 2 + 1))
                .unwrap_or_else(|| area.width.saturating_sub(1 + 6 + 2 + 1));
            buf.set_stringn(
                area.x + 1 + 6 + 2,
                y,
                &summary,
                usize::from(budget),
                Style::default()
                    .fg(state_color(AgentState::Blocked))
                    .add_modifier(Modifier::BOLD),
            );
        }
        // The fleet toggle: arm auto-approve for every agent, or disarm
        // all when everything is already on. An accent-filled pill when the
        // whole fleet is armed, a quiet one otherwise — same vocabulary as
        // the per-card chip.
        if let Some(cols) = auto_all_cols(area.width) {
            let all_on = !self.entries.is_empty() && self.entries.iter().all(|e| e.auto_approve);
            buf.set_string(
                area.x + cols.start,
                y,
                AUTO_ALL,
                chip(all_on, self.hovered_auto_all, false),
            );
        }
        y += 2;

        // The empty panel invites instead of gaping: a quiet centered
        // hint, its launch key in the same key-accent, label-muted
        // vocabulary as the status bar's hints. The pinned `+ new agent`
        // button below stays the mouse-first way in.
        if self.entries.is_empty() {
            let hint = "no agents yet";
            let hint_y = (area.y + area.height / 2).saturating_sub(1).max(y);
            if hint_y < bottom {
                let x = area.x + area.width.saturating_sub(hint.chars().count() as u16) / 2;
                buf.set_stringn(x, hint_y, hint, usize::from(area.right() - x), normal());
            }
            let (key, label) = ("c", " new agent");
            let keys_y = hint_y + 2;
            if keys_y < bottom {
                let drawn = (key.chars().count() + label.chars().count()) as u16;
                let x = area.x + area.width.saturating_sub(drawn) / 2;
                let key_style = Style::default()
                    .fg(crate::style::ACCENT)
                    .add_modifier(Modifier::BOLD);
                buf.set_stringn(x, keys_y, key, usize::from(area.right() - x), key_style);
                let after = x + key.chars().count() as u16;
                if after < area.right() {
                    buf.set_stringn(
                        after,
                        keys_y,
                        label,
                        usize::from(area.right() - after),
                        muted(),
                    );
                }
            }
            return;
        }

        for row in sidebar_rows(self.entries, self.focused) {
            if y >= bottom {
                break;
            }
            // The card's surface, resolved once per row: raised off the
            // base canvas — or, on the focused card, the light selected
            // fill. The inversion, not the edge bar alone, is what carries
            // "you are here". Every arm below keys its text off `inverted`
            // and `sub` (the quiet tier matching the surface), so a card
            // can't end up light-filled with dark-surface text.
            let inverted = match row {
                SidebarRow::EntryName(index)
                | SidebarRow::EntryDetail(index)
                | SidebarRow::EntryTelemetry(index) => {
                    let inverted = self.focused == Some(index);
                    let fill = if inverted {
                        selected()
                    } else {
                        Style::default().bg(SURFACE_RAISED)
                    };
                    buf.set_style(Rect::new(area.x, y, area.width, 1), fill);
                    inverted
                }
                SidebarRow::Blank => false,
            };
            let sub = if inverted { selected_muted() } else { muted() };
            match row {
                SidebarRow::EntryName(index) => {
                    let entry = &self.entries[index];
                    let is_selected = self.selected == Some(index);
                    // The jump marker keeps the raw accent even on the
                    // light fill: the brand red clears it (~4.3:1), and
                    // the moving cursor must look the same on every card
                    // it lands on.
                    let marker_style = Style::default().fg(crate::style::ACCENT);
                    if is_selected {
                        buf.set_string(area.x, y, "❯", marker_style);
                    } else if self.hovered == Some(index) {
                        // Hover affordance: a quiet marker where selection's
                        // ❯ goes.
                        buf.set_string(area.x, y, "❯", sub);
                    } else if inverted {
                        // The focus bar's top cell; the transient jump and
                        // hover markers outrank it on this row — the bar
                        // still reads from the rows below.
                        Self::draw_focus_bar(buf, area.x, y);
                    }

                    // Glyph + agent name (bold), age right-aligned and
                    // quiet. The glyph's style is tick-animated: done
                    // pulses to pull the eye. The inverted card gets the
                    // selected-surface variant — blocked red and done
                    // azure keep their hues, working/idle drop to dark
                    // text (see `state_glyph_style_selected`).
                    let glyph_style = if inverted {
                        state_glyph_style_selected(entry.state, self.tick)
                    } else {
                        state_glyph_style(entry.state, self.tick)
                    };
                    buf.set_string(
                        area.x + 2,
                        y,
                        state_glyph(entry.state, self.tick),
                        glyph_style,
                    );
                    // The right block: an optional `⧉N` workspace tag —
                    // shown once there is more than one workspace, since the
                    // flat global list has no headers to name the home — then
                    // the age, laid out from the right edge inward past a
                    // one-column margin.
                    let age = entry.age.map(format_age).unwrap_or_default();
                    let tag = if self.workspaces > 1 {
                        format!("⧉{}", entry.window + 1)
                    } else {
                        String::new()
                    };
                    let name_start = area.x + 4;
                    let mut block_left = area.x + area.width - 1;
                    if !age.is_empty() {
                        block_left = block_left.saturating_sub(age.chars().count() as u16);
                        buf.set_string(block_left, y, &age, sub);
                    }
                    if !tag.is_empty() {
                        // A one-column gap sets the tag off from the age.
                        let gap = u16::from(!age.is_empty());
                        let start = block_left.saturating_sub(gap + tag.chars().count() as u16);
                        // Drop the tag on a sidebar too narrow to place it
                        // clear of the status glyph — the name keeps the room
                        // instead, like the reason yields to the `auto` chip.
                        if start >= name_start {
                            block_left = start;
                            buf.set_string(block_left, y, &tag, sub);
                        }
                    }
                    // The name fills from its indent up to the right block:
                    // the bright tier, accented while jump-selected, dark
                    // on the inverted card (accent-on-light has no
                    // contrast; the inversion already marks the card).
                    let name_width = usize::from(block_left.saturating_sub(name_start));
                    let name_style = if inverted {
                        selected().add_modifier(Modifier::BOLD)
                    } else if is_selected {
                        Style::default()
                            .fg(crate::style::ACCENT)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        bright().add_modifier(Modifier::BOLD)
                    };
                    // The live task title beats the config name — every
                    // card saying `claude-code` says nothing.
                    let name = entry.display_name();
                    buf.set_string(name_start, y, truncate(name, name_width), name_style);
                }
                SidebarRow::EntryDetail(index) => {
                    // The reason owns the line — the glyph above already
                    // carries the state in shape and color, so the row
                    // spends its width on why, not on the state's name.
                    // The `auto` chip holds the right edge: always drawn
                    // while armed (it is a state signal), otherwise only
                    // revealed on the card under the pointer, the selected
                    // card, or the focused one — eight unarmed chips on
                    // eight at-rest cards is wallpaper, not affordance.
                    let entry = &self.entries[index];
                    if inverted {
                        Self::draw_focus_bar(buf, area.x, y);
                    }
                    let chip_cols = auto_chip_cols(area.width);
                    // A card with no reason (a fresh idle pane) falls back
                    // to the state word so the line never sits empty.
                    let reason = entry.reason.as_deref().unwrap_or(state_label(entry.state));
                    // The reason's budget always stops short of the chip
                    // columns, drawn or not — text reflowing under the
                    // pointer as the chip appears would read as flicker.
                    let end = chip_cols.as_ref().map_or(width.saturating_sub(1), |cols| {
                        usize::from(cols.start).saturating_sub(1)
                    });
                    let budget = end.saturating_sub(usize::from(CARD_INDENT));
                    if budget > 0 {
                        // The reason is the signal: the normal tier, a step
                        // above the muted ages and hints around it.
                        let reason_style = if inverted { selected() } else { normal() };
                        buf.set_stringn(
                            area.x + CARD_INDENT,
                            y,
                            truncate(reason, budget),
                            budget,
                            reason_style,
                        );
                    }
                    if let Some(cols) = chip_cols {
                        let revealed = entry.auto_approve
                            || self.hovered == Some(index)
                            || self.hovered_auto == Some(index)
                            || self.selected == Some(index)
                            || self.focused == Some(index);
                        if revealed {
                            buf.set_string(
                                area.x + cols.start,
                                y,
                                AUTO_CHIP,
                                chip(
                                    entry.auto_approve,
                                    self.hovered_auto == Some(index),
                                    inverted,
                                ),
                            );
                        }
                    }
                }
                SidebarRow::EntryTelemetry(index) => {
                    // Badge line under the detail row, same indent. The
                    // focused card gets the full line — model, context %,
                    // cost, rate limit; any other card planned here is one
                    // whose context alert escalated, and shows just that
                    // badge (see `telemetry_row_visible`).
                    let entry = &self.entries[index];
                    if inverted {
                        Self::draw_focus_bar(buf, area.x, y);
                    }
                    if let Some(telemetry) = &entry.telemetry {
                        // Card indent + one right-margin column, like the
                        // rows above. An unfocused row exists only because
                        // its context alert escalated (the row plan and
                        // this content decision share telemetry_row_visible),
                        // so the lone badge is always present. The full
                        // line renders only on the focused — inverted —
                        // card, and telemetry.rs owns the re-key for it.
                        let budget = width.saturating_sub(4).saturating_sub(1);
                        let line = if inverted {
                            telemetry_line(telemetry, true)
                        } else {
                            Line::from(
                                context_badge(telemetry, false)
                                    .into_iter()
                                    .collect::<Vec<_>>(),
                            )
                        };
                        let line = truncate_line(line, budget, sub);
                        buf.set_line(area.x + 4, y, &line, area.width.saturating_sub(4));
                    }
                }
                SidebarRow::Blank => {}
            }
            y += 1;
        }
    }
}

/// Compact age for the sidebar: seconds under a minute, then minutes,
/// hours, days. The day unit exists for the weekly rate-limit reset — a
/// multi-day span rendered "134h" outruns the footer's width budget.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Truncate `text` to at most `width` display cells, marking a cut with a
/// trailing `…`. Budgets by cells, not chars — the buffer clips by cells,
/// and a double-width char (names and reasons copy agent output verbatim)
/// counted as one would paint past its budget into whatever sits to the
/// right. A wide char that doesn't fit whole is dropped whole. Shared by
/// the exited card and the launcher, whose columns have the same hazard.
pub(crate) fn truncate(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = text[..cell_cut(text, width - 1)].to_string();
    out.push('…');
    out
}

/// The byte end of the widest prefix of `text` fitting `budget` display
/// cells — a wide char that doesn't fit whole is dropped whole. The single
/// owner of the cell-counting cut both [`truncate`] and [`truncate_line`]
/// rely on; two copies of this loop would drift.
fn cell_cut(text: &str, budget: usize) -> usize {
    let mut used = 0;
    let mut end = 0;
    for (at, ch) in text.char_indices() {
        let next = at + ch.len_utf8();
        let cells = Span::raw(&text[at..next]).width();
        if used + cells > budget {
            break;
        }
        used += cells;
        end = next;
    }
    end
}

/// [`truncate`] for a styled line, keeping each span's style: a cut is
/// marked with the same trailing `…` as the plain-text rows, in the
/// caller's quiet tier (`ellipsis`) so it matches the row's surface. A
/// hard clip would leave a badge reading as a smaller number than it is
/// (`$12.34` → `$12`), which misleads instead of signalling "narrow".
/// Budgets by display cells, not chars — the buffer clips by cells, and a
/// double-width char (the feed copies `display_name` verbatim) must not
/// push the `…` off the edge.
fn truncate_line(line: Line<'static>, width: usize, ellipsis: Style) -> Line<'static> {
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
            let end = cell_cut(&span.content, budget);
            spans.push(Span::styled(span.content[..end].to_string(), span.style));
            budget = 0;
        }
    }
    spans.push(Span::styled("…", ellipsis));
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
    fn working_sinks_below_idle() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("claude".into());
        // The working pane has been at it far longer; the idle one still
        // leads — the tier is the signal, age only orders within it.
        session.set_reading(
            a,
            AgentState::Working,
            Some("running tests".into()),
            now - Duration::from_secs(600),
        );
        session.set_reading(b, AgentState::Idle, None, now - Duration::from_secs(5));
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        assert_eq!(entries[0].pane, b);
        assert_eq!(entries[0].state, AgentState::Idle);
        assert_eq!(entries[1].pane, a);
        assert_eq!(entries[1].state, AgentState::Working);
    }

    #[test]
    fn equal_priority_keeps_pane_order() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.pane_mut(b).unwrap().command = Some("claude".into());
        let at = now - Duration::from_secs(30);
        session.set_reading(a, AgentState::Working, Some("w".into()), at);
        session.set_reading(b, AgentState::Working, Some("w".into()), at);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Same state, same age: the pane id breaks the tie, so equal
        // cards hold a stable position instead of reshuffling.
        assert_eq!(entries[0].pane, a);
        assert_eq!(entries[1].pane, b);
    }

    #[test]
    fn entries_rank_across_workspaces_ignoring_grouping() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Blocked, Some("q".into()), now);

        // The ranking is global: the blocked agent in window 1 outranks the
        // idle one in window 0, because no workspace term leads the sort key.
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        assert_eq!(entries[0].window, 1);
        assert_eq!(entries[0].state, AgentState::Blocked);
        assert_eq!(entries[1].window, 0);
        assert_eq!(entries[1].state, AgentState::Idle);
    }

    #[test]
    fn rows_are_a_flat_headerless_plan_across_workspaces() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Blocked, Some("q".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Two workspaces, one flat list: each card is name + detail + blank,
        // nothing else — no header or placeholder rows between them.
        let rows = sidebar_rows(&entries, None);
        assert_eq!(
            rows,
            vec![
                SidebarRow::EntryName(0),
                SidebarRow::EntryDetail(0),
                SidebarRow::Blank,
                SidebarRow::EntryName(1),
                SidebarRow::EntryDetail(1),
                SidebarRow::Blank,
            ]
        );
    }

    #[test]
    fn format_age_scales_units() {
        assert_eq!(format_age(Duration::from_secs(12)), "12s");
        assert_eq!(format_age(Duration::from_secs(90)), "1m");
        assert_eq!(format_age(Duration::from_secs(3700)), "1h");
        assert_eq!(format_age(Duration::from_secs(86_399)), "23h");
        assert_eq!(format_age(Duration::from_secs(86_400)), "1d");
        assert_eq!(format_age(Duration::from_secs(86_400 * 6 + 3600)), "6d");
        assert_eq!(format_age(Duration::ZERO), "0s");
    }

    #[test]
    fn selection_wraps_both_ways_and_activates() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut state = SidebarState::new();
        assert_eq!(state.selected(&entries), Some(0));

        state.select_prev(&entries);
        assert_eq!(state.selected(&entries), Some(2));
        state.select_next(&entries);
        assert_eq!(state.selected(&entries), Some(0));

        assert_eq!(
            state.activate(&entries),
            Some(Message::JumpToPane(entries[0].pane))
        );
        assert_eq!(SidebarState::new().selected(&[]), None);
        assert_eq!(SidebarState::new().activate(&[]), None);
    }

    #[test]
    fn selection_follows_its_pane_across_a_retriage() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let state = SidebarState::anchored(&entries);
        let chosen = entries[0].pane;
        assert_eq!(chosen, ids[1], "the blocked pane leads the triage");

        // The chosen pane's ask gets answered (say, auto-approved) and it
        // goes working: it sinks to the bottom tier and a different pane
        // takes row zero.
        session.set_reading(ids[1], AgentState::Working, Some("resumed".into()), now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        assert_ne!(entries[0].pane, chosen, "the retriage must move the pane");

        // The highlight, the jump, and anything resolved through
        // `selected()` — the binary's auto-approve toggle included — stay
        // on the chosen pane, not on whichever pane now holds its old row.
        let selected = state.selected(&entries).unwrap();
        assert_eq!(entries[selected].pane, chosen);
        assert_eq!(state.activate(&entries), Some(Message::JumpToPane(chosen)));
    }

    #[test]
    fn selection_falls_back_to_the_held_row_when_its_pane_closes() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut state = SidebarState::new();
        state.select_next(&entries);
        let gone = entries[1].pane;

        // The anchored pane vanishes: the selection keeps the row it held.
        let remaining: Vec<SidebarEntry> = entries
            .iter()
            .filter(|entry| entry.pane != gone)
            .cloned()
            .collect();
        assert_eq!(state.selected(&remaining), Some(1));

        // And that row clamps when the list shrinks below it.
        assert_eq!(state.selected(&remaining[..1]), Some(0));
        assert_eq!(state.selected(&[]), None);
    }

    #[test]
    fn header_shows_blocked_count_and_cards_lead_with_the_reason() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);

        // Quiet lowercase header with the blocked count on the right.
        let header = buffer_row(&buf, 0);
        assert!(header.starts_with(" agents"), "header: {header}");
        assert!(header.contains("1 blocked"), "header: {header}");
        assert!(header.ends_with("auto-yes"), "header: {header}");

        // Cards start after a blank row: the blocked card first (glyph, bold
        // name, right-aligned age; the reason below — the glyph already
        // says "blocked", so the detail row doesn't), then a blank spacer
        // before the next card.
        let name_row = buffer_row(&buf, 2);
        assert!(name_row.starts_with("  ◉ claude-code"), "row: {name_row}");
        assert!(name_row.ends_with("30s"), "row: {name_row}");
        let detail_row = buffer_row(&buf, 3);
        assert!(detail_row.starts_with("    Approve"), "row: {detail_row}");
        assert_eq!(buffer_row(&buf, 4), "");
        // The second card (after the spacer) is the done pane; its reason
        // still distinguishes cards now that every card shares the name
        // "claude-code".
        assert!(
            buffer_row(&buf, 6).starts_with("    finished"),
            "second card is the done pane: {}",
            buffer_row(&buf, 6)
        );

        // The reason is the signal, so it takes the normal tier — a step
        // above the muted ages, below the bright name. The state color
        // lives on the glyph, not spelled out twice per card.
        assert_eq!(buf.cell((4, 3)).unwrap().style().fg, normal().fg);
        assert_eq!(buf.cell((4, 2)).unwrap().style().fg, bright().fg);
    }

    #[test]
    fn reasonless_cards_fall_back_to_the_state_word() {
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // No reason reported: the detail line says "idle" rather than
        // sitting empty under the name.
        assert_eq!(buffer_row(&buf, 3), "    idle");
    }

    #[test]
    fn done_glyph_pulses_reversed_across_ticks() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // The done card is second (blocked card at rows 2-3, spacer at 4),
        // its ✓ glyph at column 2 of row 5.
        let glyph_at = |tick: u64| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
            Sidebar::new(&entries, None, None, session.window_count(), tick)
                .render(Rect::new(0, 0, 32, 14), &mut buf);
            assert_eq!(buf.cell((2, 5)).unwrap().symbol(), "✓");
            buf.cell((2, 5)).unwrap().style()
        };
        let off = glyph_at(0);
        let on = glyph_at(4);
        assert!(!off.add_modifier.contains(Modifier::REVERSED));
        assert!(on.add_modifier.contains(Modifier::REVERSED));
        // Both phases keep the explicit done color — the pulse never dims
        // the glyph or drops its foreground.
        assert_eq!(off.fg, Some(state_color(AgentState::Done)));
        assert_eq!(on.fg, Some(state_color(AgentState::Done)));
    }

    #[test]
    fn focused_card_carries_an_accent_bar_down_its_edge() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        // Focus the done pane — entry index 1, card rows 5 (name) and 6
        // (detail).
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(1))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        for y in [5, 6] {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "▍", "row {y}");
            assert_eq!(
                buf.cell((0, y)).unwrap().style().fg,
                Some(crate::style::ACCENT),
                "row {y}"
            );
        }
        // The unfocused cards' edge column stays empty.
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((0, 8)).unwrap().symbol(), " ");
    }

    #[test]
    fn jump_selection_outranks_the_focus_bar_on_the_name_row() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, Some(1), None, session.window_count(), 0)
            .focused(Some(1))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // Jump mode's ❯ takes the name row's marker cell; the bar still
        // shows on the detail row below.
        assert_eq!(buf.cell((0, 5)).unwrap().symbol(), "❯");
        assert_eq!(buf.cell((0, 6)).unwrap().symbol(), "▍");
    }

    #[test]
    fn renders_cards_in_triage_order() {
        // Creation order deliberately scrambled: working, idle, done,
        // blocked. The rendered cards read top-down as blocked, done, idle,
        // working — the triage order, not the creation order.
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        let b = session.split(a, SplitDirection::Horizontal).unwrap();
        let c = session.split(b, SplitDirection::Vertical).unwrap();
        let d = session.split(c, SplitDirection::Vertical).unwrap();
        for id in [a, b, c, d] {
            session.pane_mut(id).unwrap().command = Some("claude".into());
        }
        session.set_reading(a, AgentState::Working, Some("tests".into()), now);
        session.set_reading(b, AgentState::Idle, None, now);
        session.set_reading(c, AgentState::Done, Some("finished".into()), now);
        session.set_reading(d, AgentState::Blocked, Some("Approve?".into()), now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 16), &mut buf);

        // Two-line cards with a blank between: detail rows at 3, 6, 9, 12,
        // each leading with its reason (the idle card has none and falls
        // back to the state word).
        for (y, detail) in [(3, "Approve?"), (6, "finished"), (9, "idle"), (12, "tests")] {
            let row = buffer_row(&buf, y);
            assert!(
                row.starts_with(&format!("    {detail}")),
                "row {y} should lead with {detail}: {row}"
            );
        }
    }

    #[test]
    fn workspace_tag_yields_to_the_glyph_on_a_narrow_sidebar() {
        // Two workspaces so each card carries a `⧉N` tag.
        let now = Instant::now();
        let mut session = Session::new();
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Blocked, Some("q".into()), now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Idle, None, now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);

        // A roomy sidebar shows the tag.
        let mut wide = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut wide);
        assert!(
            buffer_row(&wide, 2).contains('⧉'),
            "wide card should carry a tag: {}",
            buffer_row(&wide, 2)
        );

        // The narrowest renderable sidebar drops the tag rather than
        // overpainting the status glyph at column 2.
        let mut narrow = Buffer::empty(Rect::new(0, 0, 8, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 8, 14), &mut narrow);
        let row = buffer_row(&narrow, 2);
        assert!(!row.contains('⧉'), "narrow card must drop the tag: {row}");
        assert_eq!(
            narrow.cell((2, 2)).unwrap().symbol(),
            state_glyph(AgentState::Blocked, 0),
            "the status glyph must survive"
        );
    }

    #[test]
    fn card_title_prefers_the_panes_terminal_title_over_the_agent_name() {
        let now = Instant::now();
        let (mut session, panes) = populated_session(now);
        // The blocked pane broadcasts its task; the others never set one.
        session.set_title(panes[1], Some("fixing the auth bug".into()));

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);

        // The blocked card (sorted first) is labeled by its task…
        let titled = buffer_row(&buf, 2);
        assert!(
            titled.starts_with("  ◉ fixing the auth bug"),
            "row: {titled}"
        );
        // …and a card without a title keeps the agent-name fallback.
        assert!(
            buffer_row(&buf, 5).contains("claude-code"),
            "fallback row: {}",
            buffer_row(&buf, 5)
        );
    }

    #[test]
    fn blank_or_whitespace_title_falls_back_to_the_agent_name() {
        // The binary trims and empty-filters titles before set_title, but
        // display_name is the last line of defense: a blank title that
        // reaches an entry by any other path must not blank the chrome.
        let now = Instant::now();
        let (mut session, panes) = populated_session(now);
        session.set_title(panes[1], Some("   ".into()));
        session.set_title(panes[2], Some("".into()));
        session.set_title(panes[0], Some("  real task  ".into()));

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        for entry in &entries {
            if entry.pane == panes[0] {
                // A padded title is shown trimmed, not blank-wrapped.
                assert_eq!(entry.display_name(), "real task");
            } else {
                assert_eq!(entry.display_name(), "claude-code", "pane {:?}", entry.pane);
            }
        }
    }

    #[test]
    fn card_without_a_title_falls_back_to_the_session_name_then_the_agent() {
        // A session whose first prompt is a slash command never gets a
        // title summary from the agent, but the statusline feed still names
        // the session — that name beats the bare agent name. A broadcast
        // title still outranks it, and a nameless pane keeps the agent
        // fallback (blank-name guarding is the model's, tested there).
        let now = Instant::now();
        let (mut session, panes) = populated_session(now);
        session.set_session_name(panes[0], None, Some("Fix the auth flow".into()));
        session.set_session_name(panes[1], None, Some("Ship the sidebar".into()));
        session.set_title(panes[1], Some("fixing the auth bug".into()));

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        for entry in &entries {
            let expected = match entry.pane {
                p if p == panes[0] => "Fix the auth flow",
                p if p == panes[1] => "fixing the auth bug",
                _ => "claude-code",
            };
            assert_eq!(entry.display_name(), expected, "pane {:?}", entry.pane);
        }
    }

    #[test]
    fn auto_all_pill_sits_in_the_header_accent_filled_when_fleet_armed() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let mut entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 31, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 31, 14), &mut buf);

        // The blocked count sits inline after the label; the pill holds
        // the right edge (its trailing pad space trims off the row text).
        let header = buffer_row(&buf, 0);
        assert!(header.starts_with(" agents  1 blocked"), "header: {header}");
        assert!(header.ends_with("auto-yes"), "header: {header}");
        // Mixed fleet (none armed here): a quiet muted pill — reversed is
        // the button shape, not a hover effect.
        let cols = auto_all_cols(31).unwrap();
        let off = buf.cell((cols.start, 0)).unwrap().style();
        assert_eq!(off.fg, muted().fg);
        assert!(off.add_modifier.contains(Modifier::REVERSED));

        // Arm the whole fleet: the accent fills the pill.
        for entry in &mut entries {
            entry.auto_approve = true;
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 31, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 31, 14), &mut buf);
        let on = buf.cell((cols.start, 0)).unwrap().style();
        assert_eq!(on.fg, Some(crate::style::ACCENT));
        assert!(on.add_modifier.contains(Modifier::BOLD));
        assert!(on.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn auto_all_cols_guard_narrow_widths() {
        // Right-aligned one column in from the edge, 10 wide, clear of the
        // inline blocked count on the left.
        assert_eq!(auto_all_cols(31), Some(20..30));
        assert_eq!(auto_all_cols(30), Some(19..29));
        assert_eq!(auto_all_cols(29), None);
        assert_eq!(auto_all_cols(0), None);
    }

    #[test]
    fn auto_chip_cols_right_align_and_guard_narrow_widths() {
        // Right-aligned one column in from the edge.
        assert_eq!(auto_chip_cols(31), Some(24..30));
        assert_eq!(auto_chip_cols(40), Some(33..39));
        // One width-only threshold — the card indent plus the reason's
        // reserve — so the chip never starves the reason of its sliver.
        assert_eq!(auto_chip_cols(20), Some(13..19));
        assert_eq!(auto_chip_cols(19), None);
        assert_eq!(auto_chip_cols(0), None);
    }

    #[test]
    fn armed_chip_always_shows_unarmed_chip_waits_for_a_reveal() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let mut entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Auto-approve the first (blocked) card; leave the rest off.
        entries[0].auto_approve = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        // The armed chip is a state signal: always drawn, an accent-filled
        // pill (bold, so it survives no-color terminals).
        let detail = buffer_row(&buf, 3);
        assert!(detail.ends_with("auto"), "armed chip missing: {detail}");
        let cols = auto_chip_cols(40).unwrap();
        let on = buf.cell((cols.start, 3)).unwrap().style();
        assert_eq!(on.fg, Some(crate::style::ACCENT));
        assert!(on.add_modifier.contains(Modifier::BOLD));
        assert!(on.add_modifier.contains(Modifier::REVERSED));
        // An unarmed chip on an at-rest card is wallpaper, not affordance:
        // it stays hidden until the card is hovered, selected, or focused.
        let other = buffer_row(&buf, 6);
        assert!(!other.contains("auto"), "unarmed chip drawn: {other}");
        // A gutter column separates the truncated reason from the chip.
        assert_eq!(buf.cell((cols.start - 1, 3)).unwrap().symbol(), " ");
    }

    #[test]
    fn unarmed_chip_reveals_on_hover_selection_and_focus() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let cols = auto_chip_cols(40).unwrap();
        let render = |sidebar: Sidebar| -> Buffer {
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
            sidebar.render(Rect::new(0, 0, 40, 14), &mut buf);
            buf
        };
        // Hovering the card reveals its quiet pill — that's where the
        // pointer finds the toggle.
        let buf = render(Sidebar::new(
            &entries,
            None,
            Some(0),
            session.window_count(),
            0,
        ));
        assert!(buffer_row(&buf, 3).ends_with("auto"));
        let style = buf.cell((cols.start, 3)).unwrap().style();
        assert_eq!(style.fg, muted().fg);
        assert!(style.add_modifier.contains(Modifier::REVERSED));
        // The jump-mode selection reveals it for the keyboard (`a`
        // toggles the selected card).
        let buf = render(Sidebar::new(
            &entries,
            Some(1),
            None,
            session.window_count(),
            0,
        ));
        assert!(buffer_row(&buf, 6).ends_with("auto"));
        // The focused card shows its controls too.
        let buf =
            render(Sidebar::new(&entries, None, None, session.window_count(), 0).focused(Some(2)));
        assert!(buffer_row(&buf, 9).ends_with("auto"));
        // And everywhere else the row stays chip-free.
        assert!(!buffer_row(&buf, 3).contains("auto"));
    }

    #[test]
    fn auto_chip_hover_underlines_the_pill() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .hovered_auto(Some(0))
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        let cols = auto_chip_cols(40).unwrap();
        // Hovering the chip itself both reveals it and underlines it —
        // underline stays visible inside the reversed pill.
        let style = buf.cell((cols.start, 3)).unwrap().style();
        assert!(style.add_modifier.contains(Modifier::REVERSED));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        // Hovering one chip leaves the others hidden.
        assert!(!buffer_row(&buf, 6).contains("auto"));
    }

    #[test]
    fn auto_chip_gives_way_to_the_reason_on_a_narrow_sidebar() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let mut entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Even armed — the strongest claim a chip has to the row — the
        // reason wins below the width threshold.
        entries[0].auto_approve = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 19, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 19, 14), &mut buf);
        let detail = buffer_row(&buf, 3);
        assert!(detail.starts_with("    Approve"), "reason lost: {detail}");
        for y in 0..14 {
            let row = buffer_row(&buf, y);
            assert!(!row.contains("auto"), "chip on a narrow row: {row}");
        }
    }

    #[test]
    fn focused_card_grows_the_full_badge_line_and_at_rest_cards_do_not() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        // Feed healthy telemetry to the blocked pane only.
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

        // Unfocused with a healthy context reading, the card keeps its
        // two-line shape — the full badge line is the focused card's.
        assert_eq!(
            sidebar_rows(&entries, None),
            vec![
                SidebarRow::EntryName(0),
                SidebarRow::EntryDetail(0),
                SidebarRow::Blank,
                SidebarRow::EntryName(1),
                SidebarRow::EntryDetail(1),
                SidebarRow::Blank,
                SidebarRow::EntryName(2),
                SidebarRow::EntryDetail(2),
                SidebarRow::Blank,
            ]
        );

        // Focus the telemetry-fed card: its card alone grows the row.
        let rows = sidebar_rows(&entries, Some(0));
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
            .focused(Some(0))
            .render(Rect::new(0, 0, 40, 16), &mut buf);
        // The badge line sits under the focused card's detail row, at the
        // card indent, with the muted separators.
        assert_eq!(buffer_row(&buf, 4), "▍   Opus · 62% context · $1.23");
        // The focused card is the inverted surface, so its quiet badges
        // take the selected surface's dark-muted tier — the healthy
        // context badge too.
        assert_eq!(buf.cell((4, 4)).unwrap().style().fg, selected_muted().fg);
        // The next card starts after one blank, one row lower than before.
        assert_eq!(buffer_row(&buf, 5), "");
        assert!(
            buffer_row(&buf, 7).starts_with("    finished"),
            "second card detail: {}",
            buffer_row(&buf, 7)
        );
    }

    #[test]
    fn truncate_budgets_by_cells_not_chars() {
        // Five double-width chars are ten cells; a five-cell budget keeps
        // two whole chars (four cells) plus the one-cell ellipsis.
        assert_eq!(truncate("修复认证模", 5), "修复…");
        assert_eq!(truncate("修复认证模", 10), "修复认证模");
        assert_eq!(truncate("plain", 5), "plain");
        assert_eq!(truncate("plainer", 5), "plai…");
        // Zero width paints nothing — an `…` would land in a cell the
        // budget doesn't own (the age column on the narrowest sidebar).
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn wide_char_names_stay_inside_their_cell_budget() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        // The live task title wins the name row (display_name), and agent
        // task titles carry wide chars verbatim. Counted in chars it would
        // paint over the right-aligned age and everything past it.
        session.set_title(
            ids[1],
            Some("修复认证模块的错误处理逻辑然后重新运行测试".into()),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 24, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 24, 16), &mut buf);
        let all: String = (0..16).map(|y| buffer_row(&buf, y) + "\n").collect();
        assert!(
            all.contains("30s"),
            "age overwritten by a wide name:\n{all}"
        );
        assert!(all.contains('…'), "cut not marked:\n{all}");
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
        // survives to read as a smaller number than it is. (Focused, so
        // the full line renders at all.)
        let mut buf = Buffer::empty(Rect::new(0, 0, 26, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(0))
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
            .focused(Some(0))
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
    fn no_workspace_headers_render_even_for_multi_agent_workspaces() {
        let now = Instant::now();
        let mut session = Session::new();
        // Window 0: a single agent. Window 1: two agents — the old grouped
        // view gave the pair a header; the flat global list never does.
        let a = session.focused().unwrap();
        session.pane_mut(a).unwrap().command = Some("claude".into());
        session.set_reading(a, AgentState::Idle, None, now);
        let b = session.new_window();
        session.pane_mut(b).unwrap().command = Some("claude".into());
        session.set_reading(b, AgentState::Working, Some("go".into()), now);
        let c = session.split(b, SplitDirection::Horizontal).unwrap();
        session.pane_mut(c).unwrap().command = Some("claude".into());
        session.set_reading(c, AgentState::Idle, None, now);

        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 20));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 20), &mut buf);

        let rows: Vec<String> = (0..20).map(|y| buffer_row(&buf, y)).collect();
        assert!(
            !rows.iter().any(|r| r.trim().starts_with("workspace")),
            "the flat list draws no workspace headers, rows: {rows:#?}"
        );
    }

    #[test]
    fn cards_sit_on_raised_surfaces_over_the_base_canvas() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // The header row sits on the base canvas.
        assert_eq!(buf.cell((5, 0)).unwrap().style().bg, Some(SURFACE_BASE));
        // Card rows are raised, edge to edge — marker column and right
        // margin included, so the card reads as one block.
        for (x, y) in [(0, 2), (5, 2), (31, 2), (5, 3)] {
            assert_eq!(
                buf.cell((x, y)).unwrap().style().bg,
                Some(SURFACE_RAISED),
                "cell ({x},{y})"
            );
        }
        // The blank spacer between cards drops back to the canvas.
        assert_eq!(buf.cell((5, 4)).unwrap().style().bg, Some(SURFACE_BASE));
        // Ages stay on the muted tier.
        assert_eq!(buf.cell((28, 2)).unwrap().style().fg, muted().fg);
    }

    #[test]
    fn focused_card_is_a_full_inverted_surface() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        // Focus the done card — entry 1, rows 5 (name) and 6 (detail).
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(1))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // The light fill covers both card rows edge to edge…
        for (x, y) in [(0, 5), (4, 5), (31, 5), (4, 6), (31, 6)] {
            assert_eq!(
                buf.cell((x, y)).unwrap().style().bg,
                selected().bg,
                "cell ({x},{y})"
            );
        }
        // …with dark text on it: the bold name and the reason, the age on
        // the dark-muted tier. The done glyph keeps its azure — a hue that
        // clears the light fill — so the state color survives inversion.
        let name = buf.cell((4, 5)).unwrap().style();
        assert_eq!(name.fg, selected().fg);
        assert!(name.add_modifier.contains(Modifier::BOLD));
        assert_eq!(buf.cell((4, 6)).unwrap().style().fg, selected().fg);
        assert_eq!(
            buf.cell((2, 5)).unwrap().style().fg,
            Some(state_color(AgentState::Done))
        );
        assert_eq!(buf.cell((29, 5)).unwrap().style().fg, selected_muted().fg);
        // The done pulse survives the remap: the reversed phase flips the
        // glyph without ever dropping its foreground.
        let mut pulse = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 4)
            .focused(Some(1))
            .render(Rect::new(0, 0, 32, 14), &mut pulse);
        let glyph = pulse.cell((2, 5)).unwrap().style();
        assert!(glyph.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(glyph.fg, Some(state_color(AgentState::Done)));
        // The unfocused cards keep the raised surface and bright names.
        assert_eq!(buf.cell((4, 2)).unwrap().style().bg, Some(SURFACE_RAISED));
        assert_eq!(buf.cell((4, 2)).unwrap().style().fg, bright().fg);
    }

    #[test]
    fn focused_card_keeps_blocked_red_but_drops_the_unreadable_hues() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        // Focus the blocked card (entry 0, glyph at (2,2)): a block must
        // stay red even on the one card you're parked on.
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(0))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        let glyph = buf.cell((2, 2)).unwrap().style();
        assert_eq!(glyph.fg, Some(state_color(AgentState::Blocked)));
        assert_eq!(glyph.bg, selected().bg);
        // Focus the working card (entry 2, glyph at (2,8)): yellow has no
        // contrast on the light fill, so the glyph drops to dark text and
        // its spinner motion carries the state.
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(2))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        assert_eq!(buf.cell((2, 8)).unwrap().style().fg, selected().fg);
    }

    #[test]
    fn focused_card_auto_chip_pins_both_sides_on_the_light_fill() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let mut entries = sidebar_entries(&session, &Detector::builtin(), now);
        let cols = auto_chip_cols(40).unwrap();
        // The focused card always reveals its chip; unarmed it must be the
        // dark pill (explicit fg AND bg — the reversal trick has nothing
        // dark to swap in on the light fill).
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(0))
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        let rest = buf.cell((cols.start, 3)).unwrap().style();
        assert_eq!(rest.fg, selected().bg, "dark pill, light label");
        assert_eq!(rest.bg, selected().fg);
        // Armed, the accent is the pill's background with light text —
        // the dark tier falls under 3:1 on the mid-luminance brand red.
        entries[0].auto_approve = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(0))
            .render(Rect::new(0, 0, 40, 14), &mut buf);
        let armed = buf.cell((cols.start, 3)).unwrap().style();
        assert_eq!(armed.bg, Some(crate::style::ACCENT));
        assert_eq!(armed.fg, crate::style::bright().fg);
        assert!(armed.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn focused_card_badges_keep_their_signal_on_the_light_fill() {
        let now = Instant::now();
        let (mut session, ids) = populated_session(now);
        session.set_telemetry(
            ids[1],
            Some(Telemetry {
                model: Some("Opus".into()),
                context_pct: Some(5.0),
                cost_usd: Some(1.23),
                rate_limit: None,
            }),
        );
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 16));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .focused(Some(0))
            .render(Rect::new(0, 0, 40, 16), &mut buf);
        let row = buffer_row(&buf, 4);
        assert!(row.contains("5% context"), "badge row: {row}");
        // Quiet badges take the dark-muted tier, on the light fill.
        let model = buf.cell((4, 4)).unwrap().style();
        assert_eq!(model.fg, selected_muted().fg);
        assert_eq!(model.bg, selected().bg);
        // The critical context badge stays the blocked red — danger is red
        // on every surface — bold, on the light fill. (Column arithmetic
        // counts chars, not bytes: the row leads with the 3-byte '▍'.)
        let x = row[..row.find("5%").unwrap()].chars().count() as u16;
        let badge = buf.cell((x, 4)).unwrap().style();
        assert_eq!(badge.fg, Some(state_color(AgentState::Blocked)));
        assert_eq!(badge.bg, selected().bg);
        assert!(badge.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn empty_sidebar_shows_a_quiet_centered_hint() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&[], None, None, 1, 0).render(Rect::new(0, 0, 32, 14), &mut buf);
        // The hint centers on the panel — "no agents yet" is 13 chars on a
        // 32-column panel, so it starts at column 9.
        let hint_row = buffer_row(&buf, 6);
        assert_eq!(hint_row.trim(), "no agents yet");
        assert_eq!(buf.cell((9, 6)).unwrap().symbol(), "n");
        assert_eq!(buf.cell((9, 6)).unwrap().style().fg, normal().fg);
        // The launch key below, in the hint grammar: key accented, label
        // muted, never red.
        let keys_row = buffer_row(&buf, 8);
        assert_eq!(keys_row.trim(), "c new agent");
        assert_eq!(buf.cell((10, 8)).unwrap().symbol(), "c");
        assert_eq!(
            buf.cell((10, 8)).unwrap().style().fg,
            Some(crate::style::ACCENT)
        );
        assert_eq!(buf.cell((12, 8)).unwrap().style().fg, muted().fg);
        // The hint sits on the base canvas, and the header stays.
        assert_eq!(buf.cell((9, 6)).unwrap().style().bg, Some(SURFACE_BASE));
        assert!(buffer_row(&buf, 0).starts_with(" agents"));
    }

    #[test]
    fn tiny_sidebars_render_without_panicking() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = both_limits();
        for (w, h) in [(0, 0), (1, 1), (7, 20), (8, 1), (8, 2), (9, 3), (32, 1)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            Sidebar::new(&entries, None, None, 1, 0)
                .rate_limits(Some(&limits))
                .render(area, &mut buf);
            let mut buf = Buffer::empty(area);
            Sidebar::new(&[], None, None, 1, 0)
                .rate_limits(Some(&limits))
                .render(area, &mut buf);
        }
    }

    /// A fleet reading with both windows: five-hour healthy with a reset,
    /// seven-day healthy without one.
    fn both_limits() -> roster_core::RateLimit {
        roster_core::RateLimit {
            five_hour: Some(roster_core::RateLimitWindow {
                used_pct: 62.0,
                resets_in: Some(Duration::from_secs(7500)),
            }),
            seven_day: Some(roster_core::RateLimitWindow {
                used_pct: 41.0,
                resets_in: None,
            }),
        }
    }

    /// A fleet reading with only the five-hour window at `used_pct`, no
    /// reset time.
    fn five_hour_limits(used_pct: f32) -> roster_core::RateLimit {
        roster_core::RateLimit {
            five_hour: Some(roster_core::RateLimitWindow {
                used_pct,
                resets_in: None,
            }),
            seven_day: None,
        }
    }

    #[test]
    fn fleet_footer_pins_labeled_usage_bars_to_the_panel_bottom() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = both_limits();
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(Some(&limits))
            .render(Rect::new(0, 0, 32, 14), &mut buf);

        // The two windows hold the panel's last rows — five-hour first,
        // with its reset time; seven-day percent-only — under a blank
        // spacer, with the cards untouched above.
        assert_eq!(buffer_row(&buf, 12), " 5h ▓▓▓▓░░  62% · resets 2h");
        assert_eq!(buffer_row(&buf, 13), " wk ▓▓░░░░  41%");
        assert_eq!(buffer_row(&buf, 11), "");
        assert!(buffer_row(&buf, 2).starts_with("  ◉ claude-code"));

        // Labels, empty bar cells, and the reset stay quiet chrome; the
        // healthy filled cells and percentage take the normal tier — and
        // nothing in the footer leans on DIM (the style.rs regression).
        assert_eq!(buf.cell((1, 12)).unwrap().style().fg, muted().fg);
        assert_eq!(buf.cell((5, 12)).unwrap().style().fg, normal().fg);
        assert_eq!(buf.cell((8, 12)).unwrap().style().fg, muted().fg);
        assert_eq!(buf.cell((13, 12)).unwrap().style().fg, normal().fg);
        assert_eq!(buf.cell((18, 12)).unwrap().style().fg, muted().fg);
        for y in [12, 13] {
            for x in 0..32 {
                let style = buf.cell((x, y)).unwrap().style();
                assert!(
                    !style.add_modifier.contains(Modifier::DIM),
                    "footer cell ({x},{y}) uses DIM"
                );
            }
        }
    }

    #[test]
    fn footer_colors_escalate_at_seventy_and_ninety_used() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let style_of = |used: f32| -> Style {
            let limits = five_hour_limits(used);
            let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
            Sidebar::new(&entries, None, None, session.window_count(), 0)
                .rate_limits(Some(&limits))
                .render(Rect::new(0, 0, 32, 14), &mut buf);
            let row = buffer_row(&buf, 13);
            let x = row[..row.find('%').expect("percent rendered")]
                .chars()
                .count() as u16
                - 2;
            buf.cell((x, 13)).unwrap().style()
        };
        // 69: still the normal ramp. 70: the working yellow says look
        // soon. 90: the blocked red, bold — the same escalation the
        // context badge wears, thresholds owned by roster-core.
        let healthy = style_of(69.0);
        assert_eq!(healthy.fg, normal().fg);
        assert!(!healthy.add_modifier.contains(Modifier::BOLD));
        let warn = style_of(70.0);
        assert_eq!(warn.fg, Some(state_color(AgentState::Working)));
        assert!(!warn.add_modifier.contains(Modifier::BOLD));
        let critical = style_of(90.0);
        assert_eq!(critical.fg, Some(state_color(AgentState::Blocked)));
        assert!(critical.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn footer_percent_is_floored_so_the_number_never_outruns_its_color() {
        // The color classifies the raw share; a rounded display would show
        // "90%" in the warn yellow for 89.6 — the exact threshold number
        // wearing the wrong tier. Floored, any rendered "90%" is truly red.
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = five_hour_limits(89.6);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(Some(&limits))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        let row = buffer_row(&buf, 13);
        assert!(row.contains("89%"), "row: {row}");
        let x = row[..row.find("89%").unwrap()].chars().count() as u16;
        assert_eq!(
            buf.cell((x, 13)).unwrap().style().fg,
            Some(state_color(AgentState::Working))
        );
    }

    #[test]
    fn footer_skips_unreported_windows() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = five_hour_limits(41.0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 14));
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(Some(&limits))
            .render(Rect::new(0, 0, 32, 14), &mut buf);
        // One reported window is one footer line on the last row — no
        // blank "wk" placeholder, no stray reset text.
        assert_eq!(buffer_row(&buf, 13), " 5h ▓▓░░░░  41%");
        assert_eq!(buffer_row(&buf, 12), "");
        let all: String = (0..14).map(|y| buffer_row(&buf, y) + "\n").collect();
        assert!(!all.contains("wk"), "unreported window drawn:\n{all}");
    }

    #[test]
    fn footerless_sidebars_render_exactly_the_prior_chrome() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let area = Rect::new(0, 0, 32, 14);
        // A sidebar handed no fleet reading must be cell-for-cell the
        // sidebar from before the footer existed — the builder's `None`
        // arm changes nothing a bridge-less user sees.
        let mut with_none = Buffer::empty(area);
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(None)
            .render(area, &mut with_none);
        let mut without = Buffer::empty(area);
        Sidebar::new(&entries, None, None, session.window_count(), 0).render(area, &mut without);
        assert_eq!(with_none, without);
        let all: String = (0..14).map(|y| buffer_row(&with_none, y) + "\n").collect();
        assert!(!all.contains('▓'), "phantom footer bar:\n{all}");
        assert!(!all.contains('░'), "phantom footer bar:\n{all}");
    }

    #[test]
    fn narrow_footer_lines_truncate_with_an_ellipsis() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = both_limits();
        let area = Rect::new(0, 0, 20, 14);
        let mut buf = Buffer::empty(area);
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(Some(&limits))
            .render(area, &mut buf);
        // Too narrow for the reset tail: the cut is marked like every
        // other truncated row, never a silent hard clip.
        let row = buffer_row(&buf, 12);
        assert!(row.starts_with(" 5h"), "footer line lost: {row}");
        assert!(row.ends_with('…'), "cut not marked: {row}");
    }

    #[test]
    fn footer_yields_whole_when_cards_would_starve() {
        let now = Instant::now();
        let (session, _) = populated_session(now);
        let entries = sidebar_entries(&session, &Detector::builtin(), now);
        let limits = both_limits();
        // Two windows want 3 rows; at height 7 the header and first card
        // would starve, so the footer disappears entirely rather than
        // rendering a sliver of it.
        assert_eq!(limits_footer_height(Some(&limits), 14), 3);
        assert_eq!(limits_footer_height(Some(&five_hour_limits(50.0)), 14), 2);
        assert_eq!(limits_footer_height(Some(&limits), 7), 0);
        assert_eq!(limits_footer_height(None, 14), 0);
        assert_eq!(
            limits_footer_height(Some(&roster_core::RateLimit::default()), 14),
            0
        );
        let area = Rect::new(0, 0, 32, 7);
        let mut buf = Buffer::empty(area);
        Sidebar::new(&entries, None, None, session.window_count(), 0)
            .rate_limits(Some(&limits))
            .render(area, &mut buf);
        let all: String = (0..7).map(|y| buffer_row(&buf, y) + "\n").collect();
        assert!(!all.contains('▓'), "footer on a starved sidebar:\n{all}");
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
