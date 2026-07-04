//! Multiplexer model: panes, layout tree, session state.
//!
//! Pure data and logic — no I/O, no processes, no terminal. Everything here
//! is unit-testable in isolation. See `docs/01-crates.md`.
//!
//! The [`Grid`] type also lives here for now: in the full pipeline it is
//! produced by `roster-term`, but detection and rendering only consume it,
//! and keeping it in the agent-safe world lets those crates build and test
//! without any emulator dependency.

mod grid;
mod layout;
mod session;

pub use grid::{Cell, CellStyle, Color, Cursor, Grid};
pub use layout::{Rect, SplitDirection};
pub use session::{AgentState, Pane, PaneId, Session};
