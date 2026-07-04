//! ratatui rendering: pane contents and the agent-state sidebar.
//!
//! Renders a known model into a ratatui buffer — deterministic and
//! snapshot-testable. Input is surfaced as intent messages; the binary owns
//! the side effects. See `docs/01-crates.md`.
