//! Multiplexer model: panes, layout tree, session state.
//!
//! Pure data and logic — no I/O, no processes, no terminal. Everything here
//! is unit-testable in isolation. See `docs/01-crates.md`.
//!
//! The [`Grid`] type also lives here for now: in the full pipeline it is
//! produced by `roster-term`, but detection and rendering only consume it,
//! and keeping it in the agent-safe world lets those crates build and test
//! without any emulator dependency.

mod attention;
mod context;
mod grid;
mod layout;
mod session;
mod telemetry;

pub use attention::{rank, AttentionItem, Priority};
pub use context::{context_alert, ContextAlert, CRITICAL_THRESHOLD_PCT, WARN_THRESHOLD_PCT};
pub use grid::{Cell, CellStyle, Color, Cursor, Grid};
pub use layout::{Rect, SplitDirection};
pub use session::{AgentState, Pane, PaneId, Session};
pub use telemetry::{
    fleet_rate_limit, rate_limit_alert, LimitNotice, LimitNotifier, LimitWindow, RateLimit,
    RateLimitWindow, Telemetry, LIMIT_CRITICAL_THRESHOLD_PCT, LIMIT_WARN_THRESHOLD_PCT,
};
