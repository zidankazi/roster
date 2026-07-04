//! Agent identification, state classification, and per-agent config.
//!
//! Consumes parsed [`roster_core::Grid`] snapshots and produces a
//! [`StateReading`] per agent pane. Fully testable from grid fixtures — no
//! PTY, no subprocess. See `docs/02-state-detection.md`.
//!
//! Per refresh, the flow is: [`Detector::classify`] reads one frame into a
//! raw reading; [`History`] supplies the cross-frame signals (content
//! change, recency of activity); [`Debouncer`] refuses to flip the committed
//! state on a single frame. [`PaneTracker`] bundles that per-pane loop into
//! one call.

mod config;
mod detector;
mod track;

pub use config::{AgentConfig, ConfigError, ReasonSource};
pub use detector::{AgentKind, Detector, StateReading};
pub use track::{Debouncer, History, PaneTracker};
