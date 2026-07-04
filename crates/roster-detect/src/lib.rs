//! Agent identification, state classification, and per-agent config.
//!
//! Consumes parsed [`roster_core::Grid`] snapshots and produces a
//! `StateReading { state, reason }` per agent pane. Fully testable from grid
//! fixtures — no PTY, no subprocess. See `docs/02-state-detection.md`.
