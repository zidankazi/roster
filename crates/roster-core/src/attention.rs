//! Attention ordering: which agent needs the human first.
//!
//! The product's one triage judgment, owned here so the sidebar (today)
//! and the cross-workspace attention inbox (docs/05, phase 3) rank
//! identically. Pure ranking over snapshots the caller assembles — no
//! clock, no I/O. See `docs/05-claude-native-attention.md`.

use std::cmp::Reverse;
use std::time::Duration;

use crate::session::AgentState;

/// One agent's attention-relevant snapshot, assembled by the caller.
#[derive(Clone, Copy, Debug)]
pub struct AttentionItem {
    /// The agent's committed state. Committed — debounced — readings only:
    /// ranking off raw readings would reshuffle the queue as they bounce.
    pub state: AgentState,
    /// How long the agent has sat in this state; the caller computes this
    /// against its own clock so ranking stays deterministic.
    pub waiting_for: Option<Duration>,
    /// Whether the pending ask is destructive (delete, force-push, …).
    pub destructive: bool,
}

impl AttentionItem {
    /// The item's triage key — smaller sorts first. Callers compose it with
    /// outer criteria (workspace grouping, a pane-id tie-break) in their own
    /// sort keys; [`rank`] applies it to a flat list.
    pub fn priority(&self) -> Priority {
        // The destructive sub-tier only exists inside blocked: elsewhere the
        // flag has no ask to describe, so everything shares the plain rung.
        let ask = match (self.state, self.destructive) {
            (AgentState::Blocked, true) => 0,
            _ => 1,
        };
        Priority(
            tier(self.state),
            ask,
            Reverse(self.waiting_for.unwrap_or(Duration::ZERO)),
        )
    }
}

/// A totally ordered "needs-you-ness" key; smaller means more urgent.
///
/// Within a tier the longest wait leads (a destructive ask leads the
/// blocked tier outright, and a missing duration counts as zero wait);
/// equal keys leave the order to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority(u8, u8, Reverse<Duration>);

/// The state's urgency tier — the core judgment, in one place: 🔴 blocked
/// needs a decision now; 🔵 done holds finished work awaiting review;
/// 🟢 idle may need a nudge; 🟡 working needs nothing from the human, so
/// it sinks to the bottom.
fn tier(state: AgentState) -> u8 {
    match state {
        AgentState::Blocked => 0,
        AgentState::Done => 1,
        AgentState::Idle => 2,
        AgentState::Working => 3,
    }
}

/// Indices of `items` in attention order — most urgent first, per
/// [`AttentionItem::priority`]. Ties keep the input order.
pub fn rank(items: &[AttentionItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| items[i].priority());
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(state: AgentState) -> AttentionItem {
        AttentionItem {
            state,
            waiting_for: None,
            destructive: false,
        }
    }

    fn waiting(state: AgentState, secs: u64) -> AttentionItem {
        AttentionItem {
            state,
            waiting_for: Some(Duration::from_secs(secs)),
            destructive: false,
        }
    }

    fn blocked(secs: u64, destructive: bool) -> AttentionItem {
        AttentionItem {
            state: AgentState::Blocked,
            waiting_for: Some(Duration::from_secs(secs)),
            destructive,
        }
    }

    #[test]
    fn blocked_outranks_done() {
        let items = [item(AgentState::Done), blocked(1, false)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn done_outranks_idle() {
        let items = [item(AgentState::Idle), item(AgentState::Done)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn working_sinks_below_idle() {
        let items = [item(AgentState::Working), item(AgentState::Idle)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn tiers_order_blocked_done_idle_working() {
        let items = [
            item(AgentState::Working),
            item(AgentState::Idle),
            item(AgentState::Done),
            blocked(1, false),
        ];
        assert_eq!(rank(&items), vec![3, 2, 1, 0]);
    }

    #[test]
    fn destructive_ask_outranks_older_plain_block() {
        let items = [blocked(600, false), blocked(5, true)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn longest_blocked_first() {
        let items = [blocked(10, false), blocked(300, false), blocked(60, false)];
        assert_eq!(rank(&items), vec![1, 2, 0]);
    }

    #[test]
    fn longest_wait_leads_within_every_tier() {
        let items = [
            waiting(AgentState::Done, 5),
            waiting(AgentState::Done, 90),
            waiting(AgentState::Idle, 5),
            waiting(AgentState::Idle, 90),
        ];
        assert_eq!(rank(&items), vec![1, 0, 3, 2]);
    }

    #[test]
    fn ties_keep_input_order() {
        let items = [
            item(AgentState::Working),
            blocked(30, false),
            item(AgentState::Working),
            blocked(30, false),
        ];
        assert_eq!(rank(&items), vec![1, 3, 0, 2]);
    }

    #[test]
    fn blocked_without_duration_ranks_below_timed_blocks() {
        let items = [item(AgentState::Blocked), blocked(1, false)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn destructive_flag_is_inert_outside_blocked() {
        let mut destructive_done = item(AgentState::Done);
        destructive_done.destructive = true;
        assert_eq!(
            destructive_done.priority(),
            item(AgentState::Done).priority()
        );
    }
}
