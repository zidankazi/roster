//! Session state: windows, panes, focus, and per-pane agent metadata.

use std::collections::HashMap;
use std::time::Instant;

use crate::layout::{layout, LayoutNode, Rect, RemoveOutcome, SplitDirection};

/// Stable identifier for a pane, unique within a [`Session`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u64);

impl PaneId {
    #[cfg(test)]
    pub(crate) fn from_raw(raw: u64) -> Self {
        PaneId(raw)
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
        let mut session = Session {
            windows: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 1,
        };
        session.new_window();
        session
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

    fn window_of(&self, target: PaneId) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| w.root.leaves().contains(&target))
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
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
    fn panes_lists_all_in_id_order() {
        let mut s = Session::new();
        let a = s.focused().unwrap();
        let b = s.split(a, SplitDirection::Horizontal).unwrap();
        let c = s.new_window();
        let ids: Vec<PaneId> = s.panes().iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, b, c]);
    }
}
