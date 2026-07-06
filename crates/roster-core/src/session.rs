//! Session state: windows, panes, focus, and per-pane agent metadata.

use std::collections::HashMap;
use std::time::Instant;

use crate::layout::{layout, replace_leaf, LayoutNode, Rect, RemoveOutcome, SplitDirection};

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
}

impl Pane {
    fn new(id: PaneId) -> Self {
        Pane {
            id,
            command: None,
            state: AgentState::default(),
            reason: None,
            last_change: None,
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

    /// Activate the window at `index`. Returns false when out of range.
    pub fn activate_window(&mut self, index: usize) -> bool {
        if index < self.windows.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Adopt a pane whose id was allocated elsewhere (a session server)
    /// into a fresh window, activate it, and focus the pane. Returns `None`
    /// when the id is already taken.
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
    /// [`Session::split`]. Returns `None` when the id is taken or the
    /// target does not exist.
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
    /// missing or the id is taken.
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
                // line-oriented regardless.
                let clean = command.replace('\n', " ");
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
                let root = parse_node(&mut tokens, &mut session)?;
                if !tokens.is_empty() || !root.leaves().contains(&focused) {
                    return None;
                }
                session.windows.push(Window { root, focused });
            } else if let Some(rest) = line.strip_prefix("active ") {
                session.active = rest.parse().ok()?;
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

    /// Record a detection reading for a pane. The reason is always updated;
    /// `last_change` moves only when the state actually changes value.
    /// Returns `false` if the pane does not exist.
    pub fn set_reading(
        &mut self,
        target: PaneId,
        state: AgentState,
        reason: Option<String>,
        at: Instant,
    ) -> bool {
        let Some(pane) = self.panes.get_mut(&target) else {
            return false;
        };
        if pane.state != state {
            pane.state = state;
            pane.last_change = Some(at);
        }
        pane.reason = reason;
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

/// Parse one node, registering its leaf panes into `session`.
fn parse_node(
    tokens: &mut std::collections::VecDeque<String>,
    session: &mut Session,
) -> Option<LayoutNode> {
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
            let first = parse_node(tokens, session)?;
            let second = parse_node(tokens, session)?;
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
        assert_eq!(
            restored.pane(a).unwrap().command.as_deref(),
            Some("claude")
        );
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

        let split = s.adopt_split(adopted, 101, SplitDirection::Horizontal).unwrap();
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
    fn window_cycling_wraps_both_ways_and_activates_by_index() {
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
        assert!(s.activate_window(0));
        assert_eq!(s.active_window(), Some(0));
        assert!(!s.activate_window(3));
        assert_eq!(s.active_window(), Some(0));
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
