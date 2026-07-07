//! Attention ordering: which agent needs the human first.
//!
//! Pure ranking over snapshots the caller assembles — no clock, no I/O.
//! Blocked panes come first (a pending destructive action above a plain
//! prompt, then whoever has waited longest), then working, done, idle.
//! See `docs/05-claude-native-attention.md`.

use std::time::Duration;

use crate::session::AgentState;

/// One agent's attention-relevant snapshot, assembled by the caller.
#[derive(Clone, Copy, Debug)]
pub struct AttentionItem {
    /// The agent's committed state.
    pub state: AgentState,
    /// How long the agent has been blocked, when it is; the caller computes
    /// this against its own clock so ranking stays deterministic.
    pub blocked_for: Option<Duration>,
    /// Whether the pending ask is destructive (delete, force-push, …).
    pub destructive: bool,
}

/// Indices of `items` in attention order: blocked first (destructive asks
/// above plain ones, then longest-blocked first), then working, done, idle.
/// Ties keep the input order.
pub fn rank(items: &[AttentionItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| sort_key(&items[i]));
    order
}

/// A totally ordered key where smaller means more urgent. Blocked panes
/// order by descending wait, so the missing-duration case maps to the
/// lowest urgency within its band via `Duration::ZERO`.
fn sort_key(item: &AttentionItem) -> (u8, u8, std::cmp::Reverse<Duration>) {
    let tier = match item.state {
        AgentState::Blocked => 0,
        AgentState::Working => 1,
        AgentState::Done => 2,
        AgentState::Idle => 3,
    };
    let (ask, waited) = if item.state == AgentState::Blocked {
        (
            if item.destructive { 0 } else { 1 },
            item.blocked_for.unwrap_or(Duration::ZERO),
        )
    } else {
        (0, Duration::ZERO)
    };
    (tier, ask, std::cmp::Reverse(waited))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(state: AgentState) -> AttentionItem {
        AttentionItem {
            state,
            blocked_for: None,
            destructive: false,
        }
    }

    fn blocked(secs: u64, destructive: bool) -> AttentionItem {
        AttentionItem {
            state: AgentState::Blocked,
            blocked_for: Some(Duration::from_secs(secs)),
            destructive,
        }
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
    fn blocked_outranks_working() {
        let items = [item(AgentState::Working), blocked(1, false)];
        assert_eq!(rank(&items), vec![1, 0]);
    }

    #[test]
    fn idle_sorts_last() {
        let items = [
            item(AgentState::Idle),
            item(AgentState::Done),
            item(AgentState::Working),
            blocked(1, false),
        ];
        assert_eq!(rank(&items), vec![3, 2, 1, 0]);
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
}
