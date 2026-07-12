//! Session state: windows, panes, focus, and per-pane agent metadata.
//!
//! Pure data — the binary drives it with events (split, close, focus,
//! detection readings) and reads back layout and pane state; nothing here
//! touches a terminal, a process, or a clock beyond the `Instant` values
//! callers pass in. See `docs/01-crates.md`.

use std::collections::HashMap;
use std::time::Instant;

use crate::layout::{layout, replace_leaf, LayoutNode, Rect, RemoveOutcome, SplitDirection};
use crate::telemetry::Telemetry;

/// Stable identifier for a pane, unique within a [`Session`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u64);

impl PaneId {
    /// A pane id from its raw value — for adopting panes whose identity is
    /// decided elsewhere (a session server) and for tests.
    pub fn from_raw(raw: u64) -> Self {
        PaneId(raw)
    }

    /// The raw id value.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// What an agent pane is doing, from the human's point of view.
///
/// Priority when multiple signals are on screen is
/// `Blocked > Working > Done > Idle`; classification lives in
/// `roster-detect`, this is just the vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentState {
    /// Needs the human now: a prompt, approval, or question awaits input.
    Blocked,
    /// Making progress; output is actively changing.
    Working,
    /// Just finished — worth a look. Idle prompt shortly after activity.
    Done,
    /// At rest with nothing pending.
    #[default]
    Idle,
}

/// One terminal pane and its agent metadata.
#[derive(Clone, Debug)]
pub struct Pane {
    /// The pane's stable id.
    pub id: PaneId,
    /// The command running in the pane, when known (used to identify agents).
    pub command: Option<String>,
    /// Committed agent state, as last decided by detection.
    pub state: AgentState,
    /// Human-readable explanation of the state (the question it's blocked
    /// on, a hint at what it's doing, …).
    pub reason: Option<String>,
    /// When `state` last changed value.
    pub last_change: Option<Instant>,
    /// The pane's live statusline telemetry, when the bridge feeds one.
    /// `None` for panes without the feed — and again once a feed goes
    /// stale, so the sidebar never asserts numbers nobody is reporting.
    pub telemetry: Option<Telemetry>,
    /// The terminal title the pane's program last set (OSC 0/2) — agent
    /// CLIs put their current task there. Best-effort and display-only,
    /// never a detection signal.
    pub title: Option<String>,
    /// Whether the pane holds a completed result the human has not looked at
    /// yet. Set when the pane turns done while unfocused; cleared by
    /// [`Session::mark_seen`]. While set, [`Session::set_reading`] refuses
    /// the done→idle decay, so 🔵 done sticks until the human visits the pane
    /// instead of expiring on a timer.
    pub unseen: bool,
}

impl Pane {
    fn new(id: PaneId) -> Self {
        Pane {
            id,
            command: None,
            state: AgentState::default(),
            reason: None,
            last_change: None,
            telemetry: None,
            title: None,
            unseen: false,
        }
    }
}

struct Window {
    root: LayoutNode,
    focused: PaneId,
}

/// The whole multiplexer model: windows of split panes plus focus.
///
/// Pure data — the binary feeds it events and reads back layout; nothing
/// here touches a terminal or a process.
pub struct Session {
    windows: Vec<Window>,
    active: usize,
    panes: HashMap<PaneId, Pane>,
    next_id: u64,
}

/// Adoptable pane ids stop here (exclusive): ids at or above `2^63` only
/// arise from corrupt or hostile input (servers allocate sequentially from
/// 1), and adopting one would leave `alloc_pane`'s unchecked sequential
/// `next_id` without meaningful overflow headroom.
const MAX_ADOPTABLE_ID: u64 = 1 << 63;

impl Session {
    /// A session with one window holding one pane, which has focus.
    pub fn new() -> Self {
        let mut session = Session::empty();
        session.new_window();
        session
    }

    /// A session with no windows at all — the starting point when panes
    /// are adopted from elsewhere (a session server) instead of allocated.
    pub fn empty() -> Self {
        Session {
            windows: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 1,
        }
    }

    fn alloc_pane(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        self.panes.insert(id, Pane::new(id));
        id
    }

    /// True when every pane has been closed and nothing is left to show.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Number of windows.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Index of the active window, or `None` when the session is empty.
    pub fn active_window(&self) -> Option<usize> {
        (!self.windows.is_empty()).then_some(self.active)
    }

    /// Create a new window with a fresh pane, make it active, and focus the
    /// pane.
    pub fn new_window(&mut self) -> PaneId {
        let id = self.alloc_pane();
        self.windows.push(Window {
            root: LayoutNode::Leaf(id),
            focused: id,
        });
        self.active = self.windows.len() - 1;
        id
    }

    /// Activate the next window, wrapping. No-op with fewer than two windows.
    pub fn next_window(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + 1) % self.windows.len();
        }
    }

    /// Activate the previous window, wrapping. No-op with fewer than two
    /// windows.
    pub fn prev_window(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + self.windows.len() - 1) % self.windows.len();
        }
    }

    /// Adopt a pane whose id was allocated elsewhere (a session server)
    /// into a fresh window, activate it, and focus the pane. Returns `None`
    /// when the id is already taken (ids at or above `2^63` are never adoptable).
    pub fn adopt_window(&mut self, raw: u64) -> Option<PaneId> {
        let id = self.adopt_pane(raw)?;
        self.windows.push(Window {
            root: LayoutNode::Leaf(id),
            focused: id,
        });
        self.active = self.windows.len() - 1;
        Some(id)
    }

    /// Adopt an elsewhere-allocated pane by splitting `target`, like
    /// [`Session::split`]. Returns `None` when the id is taken
    /// (ids at or above `2^63` are never adoptable) or the target does not exist.
    pub fn adopt_split(
        &mut self,
        target: PaneId,
        raw: u64,
        direction: SplitDirection,
    ) -> Option<PaneId> {
        if self.panes.contains_key(&PaneId(raw)) {
            return None;
        }
        let window_idx = self.window_of(target)?;
        let new = self.adopt_pane(raw)?;
        let window = &mut self.windows[window_idx];
        if window.root.split_leaf(target, new, direction) {
            window.focused = new;
            self.active = window_idx;
            Some(new)
        } else {
            self.panes.remove(&new);
            None
        }
    }

    /// Swap the pane `old` for a fresh pane with the elsewhere-allocated id
    /// `raw`, in place: same spot in the layout, focus follows. The new
    /// pane starts with empty metadata. Returns `None` when `old` is
    /// missing or the id is taken (ids at or above `2^63` are never adoptable).
    pub fn replace_pane(&mut self, old: PaneId, raw: u64) -> Option<PaneId> {
        if self.panes.contains_key(&PaneId(raw)) {
            return None;
        }
        let window_idx = self.window_of(old)?;
        let new = self.adopt_pane(raw)?;
        let window = &mut self.windows[window_idx];
        replace_leaf(&mut window.root, old, new);
        if window.focused == old {
            window.focused = new;
        }
        self.panes.remove(&old);
        Some(new)
    }

    fn adopt_pane(&mut self, raw: u64) -> Option<PaneId> {
        // Adopting bumps `next_id` past `raw`, and `alloc_pane` increments
        // from there unchecked — so an id near the top of the space would
        // eventually overflow it (refusing only `u64::MAX` still lets
        // `u64::MAX - 1` set `next_id` to the very edge). Real servers
        // allocate sequentially from 1; anything in the top half of the id
        // space is corrupt or hostile input, refused like a taken id. The
        // remaining 2^63 of headroom cannot be walked off sequentially.
        if raw >= MAX_ADOPTABLE_ID {
            return None;
        }
        let id = PaneId(raw);
        if self.panes.contains_key(&id) {
            return None;
        }
        self.panes.insert(id, Pane::new(id));
        self.next_id = self.next_id.max(raw + 1);
        Some(id)
    }

    /// Serialize the session's shape — windows, split trees, focus, the
    /// active window, and each pane's command — into a text blob that
    /// [`Session::restore`] can rebuild, pane ids preserved. Agent state is
    /// deliberately left out: it is re-detected from live screens.
    ///
    /// This is a persisted compatibility surface (v1); the grammar, one
    /// line per production:
    /// - `v1` — the format header, always the first line.
    /// - `window focused=<id> <node>` — one per window, in order. `<node>`
    ///   is `(l <id>)` for a leaf or `(h <ratio> <node> <node>)` /
    ///   `(v <ratio> <node> <node>)` for a split (ratio to 4 decimal places).
    /// - `active <n>` — the active window's index.
    /// - `pane <id> <command>` — one per pane with a known command; unlisted
    ///   panes default to `None` on restore.
    /// - `wname <idx> <name>` — legacy (workspace naming was removed): never
    ///   written, but accepted and dropped by [`Session::restore`] so
    ///   snapshots persisted by older rosters keep restoring. Do not delete
    ///   the parse branch as dead code.
    pub fn snapshot(&self) -> String {
        let mut out = String::from("v1\n");
        for window in &self.windows {
            out.push_str(&format!("window focused={} ", window.focused.0));
            write_node(&mut out, &window.root);
            out.push('\n');
        }
        out.push_str(&format!("active {}\n", self.active));
        let mut ids: Vec<&PaneId> = self.panes.keys().collect();
        ids.sort();
        for id in ids {
            if let Some(command) = &self.panes[id].command {
                // Commands are one line by construction; keep the format
                // line-oriented regardless. `\r` sanitized too: the
                // line-based reader would strip it before a `\n`, silently
                // altering the command and breaking snapshot stability.
                let clean = command.replace(['\n', '\r'], " ");
                out.push_str(&format!("pane {} {}\n", id.0, clean));
            }
        }
        out
    }

    /// Rebuild a session from [`Session::snapshot`] output. Returns `None`
    /// for anything malformed or a snapshot with no windows.
    pub fn restore(text: &str) -> Option<Session> {
        let mut lines = text.lines();
        if lines.next()? != "v1" {
            return None;
        }
        let mut session = Session {
            windows: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 1,
        };
        for line in lines {
            if let Some(rest) = line.strip_prefix("window focused=") {
                let (focused, node_text) = rest.split_once(' ')?;
                let focused = PaneId(focused.parse().ok()?);
                let mut tokens = tokenize(node_text);
                let root = parse_node(&mut tokens, &mut session, 0)?;
                if !tokens.is_empty() || !root.leaves().contains(&focused) {
                    return None;
                }
                session.windows.push(Window { root, focused });
            } else if let Some(rest) = line.strip_prefix("active ") {
                session.active = rest.parse().ok()?;
            } else if line.starts_with("wname ") {
                // Workspace naming was removed; tolerate (and drop) `wname`
                // lines so sessions persisted by an older roster still
                // restore instead of failing the unknown-line guard below.
            } else if let Some(rest) = line.strip_prefix("pane ") {
                let (id, command) = rest.split_once(' ')?;
                let id = PaneId(id.parse().ok()?);
                session.panes.get_mut(&id)?.command = Some(command.to_string());
            } else if !line.is_empty() {
                return None;
            }
        }
        if session.windows.is_empty() || session.active >= session.windows.len() {
            return None;
        }
        Some(session)
    }

    /// Split the pane `target` in two along `direction`, creating and
    /// focusing a new pane. Returns the new pane's id, or `None` if `target`
    /// does not exist.
    pub fn split(&mut self, target: PaneId, direction: SplitDirection) -> Option<PaneId> {
        let window_idx = self.window_of(target)?;
        let new = self.alloc_pane();
        let window = &mut self.windows[window_idx];
        if window.root.split_leaf(target, new, direction) {
            window.focused = new;
            self.active = window_idx;
            Some(new)
        } else {
            self.panes.remove(&new);
            None
        }
    }

    /// Close a pane, collapsing its split. Focus moves to the first pane of
    /// the surviving sibling subtree. Closing a window's last pane removes
    /// the window; closing the last pane of the last window empties the
    /// session. Returns `false` if the pane does not exist.
    pub fn close(&mut self, target: PaneId) -> bool {
        let Some(window_idx) = self.window_of(target) else {
            return false;
        };
        self.panes.remove(&target);
        let window = self.windows.remove(window_idx);
        let had_focus = window.focused == target;
        match window.root.remove_leaf(target) {
            RemoveOutcome::Removed(root) => {
                let focused = if had_focus {
                    root.leaves()[0]
                } else {
                    window.focused
                };
                self.windows.insert(window_idx, Window { root, focused });
            }
            RemoveOutcome::LastLeaf => {
                // `windows.remove` already ran, so every index at or after
                // window_idx shifted down by one — this covers both "closed
                // a window before the active one" and "closed the active
                // window itself" in a single adjustment, clamped so closing
                // window 0 can't underflow.
                if self.active >= window_idx && self.active > 0 {
                    self.active -= 1;
                }
            }
            RemoveOutcome::NotFound(_) => unreachable!("window_of found the pane"),
        }
        true
    }

    /// Focus a pane, activating its window if needed. Returns `false` if the
    /// pane does not exist.
    pub fn focus(&mut self, target: PaneId) -> bool {
        let Some(window_idx) = self.window_of(target) else {
            return false;
        };
        self.active = window_idx;
        self.windows[window_idx].focused = target;
        true
    }

    /// The focused pane of the active window, or `None` when empty.
    pub fn focused(&self) -> Option<PaneId> {
        self.windows.get(self.active).map(|w| w.focused)
    }

    /// Move focus to the next pane of the active window, in tree order,
    /// wrapping.
    pub fn focus_next(&mut self) {
        self.cycle_focus(1);
    }

    /// Move focus to the previous pane of the active window, in tree order,
    /// wrapping.
    pub fn focus_prev(&mut self) {
        self.cycle_focus(-1);
    }

    fn cycle_focus(&mut self, step: isize) {
        let Some(window) = self.windows.get_mut(self.active) else {
            return;
        };
        let leaves = window.root.leaves();
        let Some(pos) = leaves.iter().position(|id| *id == window.focused) else {
            return;
        };
        let len = leaves.len() as isize;
        let next = (pos as isize + step).rem_euclid(len) as usize;
        window.focused = leaves[next];
    }

    /// Rects of every pane in the active window, tiling `cols` × `rows`.
    /// Empty when the session is empty.
    pub fn layout(&self, cols: u16, rows: u16) -> Vec<(PaneId, Rect)> {
        let Some(window) = self.windows.get(self.active) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        layout(&window.root, Rect::new(0, 0, cols, rows), &mut out);
        out
    }

    /// A pane by id.
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    /// A pane by id, mutably.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.get_mut(&id)
    }

    /// All panes across all windows, in ascending id order.
    pub fn panes(&self) -> Vec<&Pane> {
        let mut all: Vec<&Pane> = self.panes.values().collect();
        all.sort_by_key(|p| p.id);
        all
    }

    /// Record a detection reading for a pane. `last_change` moves only when
    /// the state actually changes value.
    ///
    /// The reason is *usually* updated — the one exception is a suppressed
    /// reading: when the pane is holding an unseen done result (see
    /// [`Pane::unseen`]) and the incoming reading is the detector's idle
    /// decay, the reading is refused wholesale — state, `last_change`, and
    /// reason are all left untouched, so the card keeps showing the
    /// completion line. The human clears done by looking (see
    /// [`Session::mark_seen`]), not by time passing — that is the attention
    /// semantics the latch buys.
    ///
    /// A transition *into* [`AgentState::Done`] arms the latch iff the pane
    /// is not the focused one; a focused pane watched its own completion and
    /// keeps the timed decay. Returns `false` if the pane does not exist.
    pub fn set_reading(
        &mut self,
        target: PaneId,
        state: AgentState,
        reason: Option<String>,
        at: Instant,
    ) -> bool {
        // Borrow `focused` before the mutable pane borrow — `focused()` reads
        // `self`, which the `get_mut` below borrows exclusively.
        let focused = self.focused();
        let Some(pane) = self.panes.get_mut(&target) else {
            return false;
        };
        // Turning done while unfocused arms the latch; while focused clears
        // it — the human is already watching, so the timed decay is right.
        if pane.state != AgentState::Done && state == AgentState::Done {
            pane.unseen = focused != Some(target);
        }
        // A latched done refuses the idle decay wholesale: state,
        // last_change (the done age counts from the real completion, feeding
        // the sidebar ordering), and reason all stay — the card must keep
        // the completion line no matter what an idle reading carries.
        if state == AgentState::Idle && pane.state == AgentState::Done && pane.unseen {
            return true;
        }
        if pane.state != state {
            pane.state = state;
            pane.last_change = Some(at);
        }
        pane.reason = reason;
        true
    }

    /// Mark `target`'s completed result as seen, releasing the done latch:
    /// the next idle reading may decay the pane to 🟢. Called by the app for
    /// the focused pane every detection tick — focusing a pane IS the
    /// acknowledgment — and when a pane's child exits (death is its own
    /// signal; a stale done must not outrank it). A no-op for unknown panes.
    pub fn mark_seen(&mut self, target: PaneId) {
        if let Some(pane) = self.panes.get_mut(&target) {
            pane.unseen = false;
        }
    }

    /// Record a pane's live telemetry — `None` clears it (the feed went
    /// stale or away). Callers pass every reading's telemetry verbatim, so
    /// the model mirrors the tracker's aging instead of freezing the last
    /// numbers. Returns `false` if the pane does not exist.
    pub fn set_telemetry(&mut self, target: PaneId, telemetry: Option<Telemetry>) -> bool {
        let Some(pane) = self.panes.get_mut(&target) else {
            return false;
        };
        pane.telemetry = telemetry;
        true
    }

    /// Record a pane's terminal title — `None` clears it (the program
    /// reset the title or never set one). Returns `false` if the pane
    /// does not exist.
    pub fn set_title(&mut self, target: PaneId, title: Option<String>) -> bool {
        let Some(pane) = self.panes.get_mut(&target) else {
            return false;
        };
        pane.title = title;
        true
    }

    /// The index of the window containing `target`, or `None` if no window
    /// holds it.
    pub fn window_of(&self, target: PaneId) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| w.root.leaves().contains(&target))
    }

    /// The direction of the split divider under (`x`, `y`) in the active
    /// window, laid out at `cols` × `rows`. Horizontal splits own the last
    /// column of their first half; vertical splits own the first row of
    /// their second half.
    pub fn divider_at(&self, cols: u16, rows: u16, x: u16, y: u16) -> Option<SplitDirection> {
        self.windows
            .get(self.active)?
            .root
            .divider_at(Rect::new(0, 0, cols, rows), x, y)
    }

    /// Drag the divider under `from` toward `to`, resizing the split.
    /// Returns the divider's new position, or `None` when `from` holds no
    /// divider.
    pub fn drag_divider(
        &mut self,
        cols: u16,
        rows: u16,
        from: (u16, u16),
        to: (u16, u16),
    ) -> Option<(u16, u16)> {
        self.windows
            .get_mut(self.active)?
            .root
            .drag_divider(Rect::new(0, 0, cols, rows), from, to)
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

/// Append `node` as snapshot text: `(l <id>)` for leaves, `(h|v <ratio>
/// <first> <second>)` for splits.
fn write_node(out: &mut String, node: &LayoutNode) {
    match node {
        LayoutNode::Leaf(id) => out.push_str(&format!("(l {})", id.0)),
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let d = match direction {
                SplitDirection::Horizontal => 'h',
                SplitDirection::Vertical => 'v',
            };
            out.push_str(&format!("({d} {ratio:.4} "));
            write_node(out, first);
            out.push(' ');
            write_node(out, second);
            out.push(')');
        }
    }
}

/// Split snapshot node text into parens and words.
fn tokenize(text: &str) -> std::collections::VecDeque<String> {
    text.replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Split-nesting bound for [`parse_node`]. Repeatedly splitting the
/// newest pane chains one level per split, so the writer can exceed any
/// small bound — but a window cannot usefully render even a hundred
/// panes, while stack exhaustion needs tens of thousands of levels. 1024
/// leaves the writer enormous headroom and the stack none of the risk;
/// anything deeper is malformed input refused rather than recursed into
/// (the snapshot arrives over the wire, sized only by `MAX_FRAME`).
const MAX_SNAPSHOT_DEPTH: u32 = 1024;

/// Parse one node, registering its leaf panes into `session`. `depth`
/// counts split nesting from the window root, bounded by
/// [`MAX_SNAPSHOT_DEPTH`].
fn parse_node(
    tokens: &mut std::collections::VecDeque<String>,
    session: &mut Session,
    depth: u32,
) -> Option<LayoutNode> {
    if depth > MAX_SNAPSHOT_DEPTH {
        return None;
    }
    if tokens.pop_front()? != "(" {
        return None;
    }
    let kind = tokens.pop_front()?;
    let node = match kind.as_str() {
        "l" => {
            let raw: u64 = tokens.pop_front()?.parse().ok()?;
            let id = session.adopt_pane(raw)?;
            LayoutNode::Leaf(id)
        }
        "h" | "v" => {
            let ratio: f32 = tokens.pop_front()?.parse().ok()?;
            if !(0.0..=1.0).contains(&ratio) {
                return None;
            }
            let first = parse_node(tokens, session, depth + 1)?;
            let second = parse_node(tokens, session, depth + 1)?;
            LayoutNode::Split {
                direction: if kind == "h" {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                },
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }
        }
        _ => return None,
    };
    if tokens.pop_front()? != ")" {
        return None;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_session_has_one_focused_pane() {
        let s = Session::new();
        assert_eq!(s.window_count(), 1);
        let focused = s.focused().unwrap();
        assert!(s.pane(focused).is_some());
        assert_eq!(s.layout(80, 24).len(), 1);
    }

    #[test]
    fn split_creates_and_focuses_new_pane() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let second = s.split(first, SplitDirection::Horizontal).unwrap();
        assert_ne!(first, second);
        assert_eq!(s.focused(), Some(second));
        assert_eq!(s.layout(80, 24).len(), 2);
    }

    #[test]
    fn split_unknown_pane_fails_cleanly() {
        let mut s = Session::new();
        let bogus = PaneId::from_raw(999);
        assert_eq!(s.split(bogus, SplitDirection::Horizontal), None);
        assert_eq!(s.panes().len(), 1);
    }

    #[test]
    fn layout_reflects_split_directions() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let second = s.split(first, SplitDirection::Horizontal).unwrap();
        s.split(second, SplitDirection::Vertical).unwrap();
        let rects = s.layout(80, 24);
        assert_eq!(rects.len(), 3);
        let total: u32 = rects
            .iter()
            .map(|(_, r)| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(total, 80 * 24);
    }

    #[test]
    fn close_collapses_and_refocuses_sibling() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let second = s.split(first, SplitDirection::Horizontal).unwrap();
        assert!(s.close(second));
        assert_eq!(s.focused(), Some(first));
        assert_eq!(s.layout(80, 24).len(), 1);
        assert!(s.pane(second).is_none());
    }

    #[test]
    fn close_unfocused_pane_keeps_focus() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let second = s.split(first, SplitDirection::Horizontal).unwrap();
        assert!(s.close(first));
        assert_eq!(s.focused(), Some(second));
    }

    #[test]
    fn closing_last_pane_removes_window() {
        let mut s = Session::new();
        let only = s.focused().unwrap();
        assert!(s.close(only));
        assert!(s.is_empty());
        assert_eq!(s.focused(), None);
        assert!(s.layout(80, 24).is_empty());
    }

    #[test]
    fn closing_active_window_falls_back_to_previous() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let second = s.new_window();
        assert_eq!(s.active_window(), Some(1));
        assert!(s.close(second));
        assert_eq!(s.active_window(), Some(0));
        assert_eq!(s.focused(), Some(first));
    }

    #[test]
    fn close_unknown_pane_is_false() {
        let mut s = Session::new();
        assert!(!s.close(PaneId::from_raw(999)));
    }

    #[test]
    fn focus_jumps_across_windows() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        s.new_window();
        assert_eq!(s.active_window(), Some(1));
        assert!(s.focus(first));
        assert_eq!(s.active_window(), Some(0));
        assert_eq!(s.focused(), Some(first));
    }

    #[test]
    fn focus_cycles_in_tree_order() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        let b = s.split(a, SplitDirection::Horizontal).unwrap();
        let c = s.split(b, SplitDirection::Vertical).unwrap();
        s.focus(a);
        s.focus_next();
        assert_eq!(s.focused(), Some(b));
        s.focus_next();
        assert_eq!(s.focused(), Some(c));
        s.focus_next();
        assert_eq!(s.focused(), Some(a));
        s.focus_prev();
        assert_eq!(s.focused(), Some(c));
    }

    #[test]
    fn snapshot_restore_round_trips_layout_focus_and_commands() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        let b = s.split(a, SplitDirection::Horizontal).unwrap();
        let c = s.split(b, SplitDirection::Vertical).unwrap();
        s.pane_mut(a).unwrap().command = Some("claude".into());
        s.pane_mut(b).unwrap().command = Some("npx my-agent --flag x".into());
        let d = s.new_window();
        s.pane_mut(d).unwrap().command = Some("zsh".into());
        s.focus(b);

        let blob = s.snapshot();
        let restored = Session::restore(&blob).expect("restore");
        assert_eq!(restored.window_count(), 2);
        assert_eq!(restored.active_window(), s.active_window());
        assert_eq!(restored.focused(), Some(b));
        assert_eq!(restored.pane(a).unwrap().command.as_deref(), Some("claude"));
        assert_eq!(
            restored.pane(b).unwrap().command.as_deref(),
            Some("npx my-agent --flag x")
        );
        assert_eq!(restored.pane(d).unwrap().command.as_deref(), Some("zsh"));
        // The split tree survives: same rects for the same area.
        assert_eq!(restored.layout(80, 24), s.layout(80, 24));
        // Ids keep allocating past the restored ones.
        let _ = c;
        assert_eq!(restored.snapshot(), blob, "snapshot is stable");
    }

    #[test]
    fn restore_tolerates_legacy_wname_lines() {
        // Workspace naming was removed, but a session persisted by an older
        // roster carries `wname` lines. The reader must drop them, not trip
        // the unknown-line guard and fail the whole restore.
        let blob = "v1\nwindow focused=1 (l 1)\nactive 0\nwname 0 fix auth bug\npane 1 claude\n";
        let restored = Session::restore(blob).expect("legacy snapshot still restores");
        assert_eq!(restored.window_count(), 1);
        assert_eq!(
            restored.pane(PaneId(1)).unwrap().command.as_deref(),
            Some("claude")
        );
        // A fresh snapshot no longer emits the line.
        assert!(!restored.snapshot().contains("wname"));
    }

    #[test]
    fn restore_rejects_absurdly_deep_snapshots() {
        // A hostile blob can nest splits far past anything interactive
        // splitting builds; restore must refuse it as malformed instead of
        // recursing until the stack aborts the process.
        let mut node = "(l 0)".to_string();
        for id in 1..=1500u64 {
            node = format!("(h 0.5000 {node} (l {id}))");
        }
        let blob = format!("v1\nwindow focused=0 {node}\nactive 0\n");
        assert!(Session::restore(&blob).is_none());
    }

    #[test]
    fn restore_accepts_deep_but_plausible_nesting() {
        // Deeper than the old 128 bound on purpose: repeatedly splitting
        // the newest pane chains one level per split, and a snapshot the
        // writer can produce must always restore.
        let mut node = "(l 0)".to_string();
        for id in 1..=300u64 {
            node = format!("(h 0.5000 {node} (l {id}))");
        }
        let blob = format!("v1\nwindow focused=0 {node}\nactive 0\n");
        let restored = Session::restore(&blob).expect("300 splits restore");
        assert_eq!(restored.panes().len(), 301);
    }

    #[test]
    fn restore_rejects_pane_ids_without_alloc_headroom() {
        // Ids in the top half of the space would walk `next_id` toward
        // overflow on later allocs; adopting one must read as malformed.
        for raw in [u64::MAX, u64::MAX - 1, MAX_ADOPTABLE_ID] {
            let blob = format!("v1\nwindow focused={raw} (l {raw})\nactive 0\n");
            assert!(Session::restore(&blob).is_none(), "id {raw} accepted");
        }
        let ok = MAX_ADOPTABLE_ID - 1;
        let blob = format!("v1\nwindow focused={ok} (l {ok})\nactive 0\n");
        let restored = Session::restore(&blob).expect("below the cap restores");
        // The headroom holds: fresh allocation past the adopted id works.
        let mut s = restored;
        assert!(s.new_window().raw() > ok);
    }

    #[test]
    fn snapshot_round_trips_carriage_returns_in_commands() {
        let mut s = Session::new();
        let id = s.focused().unwrap();
        s.pane_mut(id).unwrap().command = Some("echo one\r\ntwo".into());
        let blob = s.snapshot();
        let restored = Session::restore(&blob).expect("restore");
        assert_eq!(
            restored.pane(id).unwrap().command.as_deref(),
            Some("echo one  two")
        );
        assert_eq!(restored.snapshot(), blob, "snapshot is stable");
    }

    #[test]
    fn restore_rejects_garbage() {
        assert!(Session::restore("").is_none());
        assert!(Session::restore("v2\n").is_none());
        assert!(Session::restore("v1\n").is_none(), "no windows");
        assert!(Session::restore("v1\nwindow focused=1 (l 1) trailing\n").is_none());
        assert!(Session::restore("v1\nwindow focused=9 (l 1)\nactive 0\n").is_none());
        assert!(Session::restore("v1\nwindow focused=1 (l 1)\nactive 5\n").is_none());
        assert!(
            Session::restore("v1\nwindow focused=1 (h 0.5 (l 1) (l 1))\nactive 0\n").is_none(),
            "duplicate pane ids"
        );
    }

    #[test]
    fn adopt_and_replace_wire_elsewhere_allocated_ids() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        let adopted = s.adopt_window(100).expect("adopt window");
        assert_eq!(adopted.raw(), 100);
        assert_eq!(s.focused(), Some(adopted));
        assert!(s.adopt_window(100).is_none(), "id taken");

        let split = s
            .adopt_split(adopted, 101, SplitDirection::Horizontal)
            .unwrap();
        assert_eq!(split.raw(), 101);
        assert_eq!(s.layout(80, 24).len(), 2);

        // Replace swaps the leaf in place and moves focus with it.
        s.focus(split);
        let swapped = s.replace_pane(split, 102).expect("replace");
        assert_eq!(s.focused(), Some(swapped));
        assert!(s.pane(split).is_none());
        assert_eq!(s.layout(80, 24).len(), 2);

        // Fresh ids allocate past adopted ones.
        let next = s.split(swapped, SplitDirection::Vertical).unwrap();
        assert!(next.raw() > 102);
        let _ = first;
    }

    #[test]
    fn window_cycling_wraps_both_ways() {
        let mut s = Session::new();
        s.new_window();
        s.new_window();
        assert_eq!(s.active_window(), Some(2));
        s.next_window();
        assert_eq!(s.active_window(), Some(0));
        s.prev_window();
        assert_eq!(s.active_window(), Some(2));
        s.prev_window();
        assert_eq!(s.active_window(), Some(1));
    }

    #[test]
    fn new_window_activates_and_isolates_layout() {
        let mut s = Session::new();
        let first = s.focused().unwrap();
        s.split(first, SplitDirection::Horizontal).unwrap();
        let solo = s.new_window();
        let rects = s.layout(80, 24);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].0, solo);
        s.next_window();
        assert_eq!(s.layout(80, 24).len(), 2);
    }

    #[test]
    fn set_reading_moves_last_change_only_on_state_change() {
        let mut s = Session::new();
        let id = s.focused().unwrap();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);

        assert!(s.set_reading(id, AgentState::Working, Some("compiling".into()), t0));
        assert_eq!(s.pane(id).unwrap().state, AgentState::Working);
        assert_eq!(s.pane(id).unwrap().last_change, Some(t0));

        assert!(s.set_reading(id, AgentState::Working, Some("running tests".into()), t1));
        assert_eq!(s.pane(id).unwrap().last_change, Some(t0));
        assert_eq!(s.pane(id).unwrap().reason.as_deref(), Some("running tests"));

        let t2 = t0 + Duration::from_secs(2);
        assert!(s.set_reading(id, AgentState::Blocked, Some("Allow edit?".into()), t2));
        assert_eq!(s.pane(id).unwrap().last_change, Some(t2));
    }

    #[test]
    fn done_while_unfocused_sticks_past_the_idle_decay() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        // Split focuses the new pane, leaving `a` unfocused.
        let _b = s.split(a, SplitDirection::Horizontal).unwrap();
        assert_ne!(s.focused(), Some(a));

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ pumpernickel".into()), t0));
        assert!(s.pane(a).unwrap().unseen, "unfocused done arms the latch");

        // The detector's later idle reading carries None; the latch refuses
        // the decay and keeps both the done state and the result line.
        let t1 = t0 + Duration::from_secs(20);
        assert!(s.set_reading(a, AgentState::Idle, None, t1));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Done);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("⏺ pumpernickel"));
    }

    #[test]
    fn suppressed_decay_does_not_move_last_change() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ done".into()), t0));
        assert_eq!(s.pane(a).unwrap().last_change, Some(t0));

        // The refused idle decay must leave last_change at the real
        // completion — the done age keeps counting for sidebar ordering.
        let t1 = t0 + Duration::from_secs(30);
        assert!(s.set_reading(a, AgentState::Idle, None, t1));
        assert_eq!(s.pane(a).unwrap().last_change, Some(t0));
    }

    #[test]
    fn done_while_focused_decays_to_idle_as_before() {
        let mut s = Session::new();
        let id = s.focused().unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(id, AgentState::Done, Some("⏺ done".into()), t0));
        assert!(
            !s.pane(id).unwrap().unseen,
            "focused done leaves the latch off"
        );

        let t1 = t0 + Duration::from_secs(10);
        assert!(s.set_reading(id, AgentState::Idle, None, t1));
        assert_eq!(s.pane(id).unwrap().state, AgentState::Idle);
        assert_eq!(s.pane(id).unwrap().reason, None);
        assert_eq!(s.pane(id).unwrap().last_change, Some(t1));
    }

    #[test]
    fn mark_seen_releases_the_latch() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ done".into()), t0));
        assert!(s.pane(a).unwrap().unseen);

        s.mark_seen(a);
        assert!(!s.pane(a).unwrap().unseen);

        // With the latch released the idle reading decays normally, reason
        // and all.
        let t1 = t0 + Duration::from_secs(10);
        assert!(s.set_reading(a, AgentState::Idle, None, t1));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Idle);
        assert_eq!(s.pane(a).unwrap().reason, None);
        assert_eq!(s.pane(a).unwrap().last_change, Some(t1));
    }

    #[test]
    fn latched_pane_shows_working_then_relatches_on_next_completion() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ first".into()), t0));
        assert!(s.pane(a).unwrap().unseen);

        // A re-run: working is never suppressed, even with the latch armed.
        let t1 = t0 + Duration::from_secs(1);
        assert!(s.set_reading(a, AgentState::Working, Some("compiling".into()), t1));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Working);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("compiling"));

        // Its next completion, still unfocused, re-arms the latch.
        let t2 = t0 + Duration::from_secs(2);
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ second".into()), t2));
        assert!(s.pane(a).unwrap().unseen);
        let t3 = t0 + Duration::from_secs(20);
        assert!(s.set_reading(a, AgentState::Idle, None, t3));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Done);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("⏺ second"));
    }

    #[test]
    fn done_reading_on_a_latched_pane_refreshes_the_reason() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ first line".into()), t0));
        assert!(s.pane(a).unwrap().unseen);

        // A Done→Done reading is not the idle decay: the reason refreshes
        // through the normal path, last_change stays at the real completion,
        // and the latch stays armed.
        let t1 = t0 + Duration::from_secs(3);
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ fuller line".into()), t1));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Done);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("⏺ fuller line"));
        assert_eq!(s.pane(a).unwrap().last_change, Some(t0));
        assert!(s.pane(a).unwrap().unseen);

        // Still latched: the later idle decay is refused, fresh reason kept.
        let t2 = t0 + Duration::from_secs(20);
        assert!(s.set_reading(a, AgentState::Idle, None, t2));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Done);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("⏺ fuller line"));
    }

    #[test]
    fn transitions_into_blocked_are_never_suppressed() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();

        let t0 = Instant::now();
        assert!(s.set_reading(a, AgentState::Done, Some("⏺ done".into()), t0));
        assert!(s.pane(a).unwrap().unseen);

        let t1 = t0 + Duration::from_secs(1);
        assert!(s.set_reading(a, AgentState::Blocked, Some("Allow edit?".into()), t1));
        assert_eq!(s.pane(a).unwrap().state, AgentState::Blocked);
        assert_eq!(s.pane(a).unwrap().reason.as_deref(), Some("Allow edit?"));
        assert_eq!(s.pane(a).unwrap().last_change, Some(t1));
    }

    #[test]
    fn mark_seen_unknown_pane_is_a_no_op() {
        let mut s = Session::new();
        s.mark_seen(PaneId::from_raw(999));
    }

    #[test]
    fn set_reading_unknown_pane_is_false() {
        let mut s = Session::new();
        assert!(!s.set_reading(
            PaneId::from_raw(999),
            AgentState::Idle,
            None,
            Instant::now()
        ));
    }

    #[test]
    fn set_telemetry_sets_and_clears_a_panes_readings() {
        let mut s = Session::new();
        let id = s.focused().unwrap();
        assert_eq!(s.pane(id).unwrap().telemetry, None);

        let telemetry = Telemetry {
            model: Some("Opus".into()),
            context_pct: Some(62.0),
            ..Telemetry::default()
        };
        assert!(s.set_telemetry(id, Some(telemetry.clone())));
        assert_eq!(s.pane(id).unwrap().telemetry, Some(telemetry));

        // A `None` reading clears — the feed aged out; the numbers go with
        // it rather than freezing.
        assert!(s.set_telemetry(id, None));
        assert_eq!(s.pane(id).unwrap().telemetry, None);

        assert!(!s.set_telemetry(PaneId::from_raw(999), None));
    }

    #[test]
    fn set_title_sets_and_clears_a_panes_title() {
        let mut s = Session::new();
        let id = s.focused().unwrap();
        assert_eq!(s.pane(id).unwrap().title, None);

        assert!(s.set_title(id, Some("fixing the auth bug".into())));
        assert_eq!(
            s.pane(id).unwrap().title.as_deref(),
            Some("fixing the auth bug")
        );

        // A reset title clears rather than freezing the last task.
        assert!(s.set_title(id, None));
        assert_eq!(s.pane(id).unwrap().title, None);

        assert!(!s.set_title(PaneId::from_raw(999), None));
    }

    #[test]
    fn dividers_are_found_and_draggable() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Horizontal).unwrap();
        // 80 wide at ratio 0.5 → first half 0..40, divider at column 39.
        assert_eq!(
            s.divider_at(80, 24, 39, 5),
            Some(SplitDirection::Horizontal)
        );
        assert_eq!(s.divider_at(80, 24, 20, 5), None);

        // Drag the divider to column 20: the first pane shrinks.
        let new_pos = s.drag_divider(80, 24, (39, 5), (20, 5)).unwrap();
        assert_eq!(new_pos, (20, 5));
        let rects = s.layout(80, 24);
        assert_eq!(rects[0].1.width, 21);
        assert_eq!(rects[1].1.width, 59);

        // Extreme drags clamp instead of collapsing a pane.
        s.drag_divider(80, 24, (20, 5), (0, 5)).unwrap();
        let rects = s.layout(80, 24);
        assert!(rects[0].1.width >= 1, "width: {}", rects[0].1.width);
    }

    #[test]
    fn vertical_divider_sits_on_the_lower_panes_top_row() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        s.split(a, SplitDirection::Vertical).unwrap();
        // 24 tall at 0.5 → first half rows 0..12, divider at row 12.
        assert_eq!(s.divider_at(80, 24, 10, 12), Some(SplitDirection::Vertical));
        assert_eq!(s.divider_at(80, 24, 10, 11), None);

        let new_pos = s.drag_divider(80, 24, (10, 12), (10, 6)).unwrap();
        assert_eq!(new_pos, (10, 6));
        let rects = s.layout(80, 24);
        assert_eq!(rects[0].1.height, 6);
        assert_eq!(rects[1].1.height, 18);
    }

    #[test]
    fn window_of_locates_panes_across_windows() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        let b = s.split(a, SplitDirection::Horizontal).unwrap();
        let c = s.new_window();
        assert_eq!(s.window_of(a), Some(0));
        assert_eq!(s.window_of(b), Some(0));
        assert_eq!(s.window_of(c), Some(1));
        assert_eq!(s.window_of(PaneId::from_raw(999)), None);
    }

    #[test]
    fn panes_lists_all_in_id_order() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        let b = s.split(a, SplitDirection::Horizontal).unwrap();
        let c = s.new_window();
        let ids: Vec<PaneId> = s.panes().iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, b, c]);
    }
}
