# 01 — crates

*Per-crate responsibilities, public surface, and boundaries. Each crate has a single job; the boundaries are what keep the agent-safe/keyboard split honest.*

The dependency direction is strictly one-way, bottom to top. Nothing lower depends on anything higher.

```
roster (binary: client, event loop, and the session server)
  ├─ roster-tui     → roster-core, roster-detect
  ├─ roster-detect  → roster-core
  ├─ roster-term    → roster-core   (produces the Grid; wraps alacritty_terminal)
  ├─ roster-proto   → (nothing)     (framed wire protocol for persistent sessions)
  ├─ roster-pty     → (nothing)
  └─ roster-core    → (nothing)     (the Grid type + multiplexer model live here)
```

---

## roster-pty — **do-at-keyboard**

Owns pseudo-terminal allocation and child-process lifecycle. This is the layer that spawns the user's agent CLI so it believes it has a real TTY.

- Use `portable-pty` — do not hand-roll PTY syscalls.
- Responsibilities: open a PTY pair, spawn a command in it, expose an async reader for output bytes and a writer for input, forward resize (SIGWINCH) when the pane geometry changes, and clean up on child exit.
- Public surface (sketch): `Pty::spawn(cmd, size) -> Pty`, `pty.reader()`, `pty.writer()`, `pty.resize(cols, rows)`, `pty.wait()`.
- **Why keyboard:** process lifecycle + signal handling + platform differences are nondeterministic and only surface at runtime. Not remotely verifiable.

## roster-term — **do-at-keyboard**

Turns the raw byte stream from a PTY into a structured screen grid. This is the emulator, and the single hardest piece — which is exactly why you reuse `alacritty_terminal` instead of writing it.

- Wrap `alacritty_terminal`'s parser/grid. Feed it bytes; it maintains a grid of cells (glyph + style), a cursor, scrollback, and the alternate-screen buffer (the thing that makes full-screen TUIs render right).
- Public surface (sketch): `Screen::new(size)`, `screen.advance(bytes)`, `screen.grid() -> &Grid`, `screen.cursor()`, `screen.resize(cols, rows)`.
- **Critical boundary:** the `Grid` type (rows of cells + cursor) is a pure model type in `roster-core`; `roster-term` produces one by wrapping `alacritty_terminal`, but the type itself has *no* dependency on `roster-pty` or the emulator. This is what makes `roster-detect` and `roster-tui` agent-safe — they consume `Grid` from `roster-core`, which can be constructed from a fixture in a test with zero PTY or subprocess involved.
- **Why keyboard:** escape-sequence edge cases, unicode width, scroll regions, and resize reflow are where the long-tail bugs live. Reusing alacritty's parser removes most, but the *integration* (feeding it, handling resize, reading back) needs live terminals to shake out.

## roster-core — **agent-safe**

The multiplexer's model, with no I/O. Pure data + logic.

- The pane tree (splits), windows/tabs, focus, geometry, and per-pane metadata (which agent, current state, last-activity timestamp, live telemetry when the statusline bridge feeds one).
- Layout math: given a terminal size and a split tree, compute each pane's rect.
- Public surface (sketch): `Session`, `Pane { id, agent, rect, state, reason, last_change, telemetry }`, `session.split(...)`, `session.focus(...)`, `session.layout(size) -> Vec<(PaneId, Rect)>`.
- **Why agent-safe:** it's a tree and some arithmetic. Fully unit-testable, no terminal, no processes. Prime candidate for unattended agent work.

## roster-detect — **agent-safe**

The differentiator. Identifies which panes are agents and classifies each one's state *and reason* from its parsed grid.

- **Agent identification:** match the pane's running command against known agent binaries (config-driven list). Optionally walk the process tree if the direct command is a shell.
- **State classification:** given a `Grid` snapshot, apply per-agent patterns to decide blocked / working / done / idle, and extract the human-readable reason (e.g. the prompt text it's blocked on).
- **Config:** load a declarative `agents.toml` (see `02-state-detection.md`) so new agents are added as data, not code.
- **Statusline telemetry:** `statusline::parse` maps Claude Code's statusline JSON payload into `roster_core::Telemetry` (model, context %, cost, five-hour rate limit) — fixture-tested against documented payloads; the never-parse-the-transcript rule from `05-claude-native-attention.md` applies.
- Public surface (sketch): `Detector::from_config(cfg)`, `detector.identify(cmd) -> Option<AgentKind>`, `detector.classify(agent, grid, history) -> StateReading { state, reason, telemetry }` (telemetry always `None` from the scrape; `PaneTracker` attaches and ages the statusline-fed value).
- **Why agent-safe:** consumes `Grid` fixtures, emits a state. You can write an enormous test suite from captured agent screens with no live process. This is the crate to pour agent effort into.

## roster-tui — **agent-safe**

Renders everything with `ratatui`: the pane contents and the sidebar.

- Draw each pane's `Grid` into its rect (a grid-of-cells → ratatui buffer blit).
- Draw the sidebar: one row per agent with color + label + reason + age, ranked globally across all workspaces by `roster_core::attention` (blocked, done, idle, then working at the bottom).
- Handle input intent (select a row, jump to pane) as *messages*, not by doing the I/O itself — the binary wires the actual pane switch.
- Public surface (sketch): `render(frame, &Session, &[StateReading])`, `Sidebar` widget, `PaneView` widget.
- **Why agent-safe:** rendering a known model into a ratatui buffer is deterministic and snapshot-testable. Agents can build and polish the sidebar look while you're away — and this is the part your UX strength should own.

## roster-proto — **agent-safe**

The wire protocol between a roster client and a persistent session server: length-prefixed binary frames over any `Read`/`Write` pair (a unix socket locally, an ssh subprocess's stdio remotely).

- Hand-rolled encoding, zero dependencies; the message set covers attach/input/output/layout control frames plus hook, statusline, and auto-approve frames.
- Public surface: `Frame`, `read_frame`, `write_frame`, `MAX_FRAME` — see `Frame` in `crates/roster-proto/src/lib.rs` for the current message set and each variant's relay semantics.
- **Why agent-safe:** pure serialization with round-trip and corruption tests; no I/O beyond the passed-in streams.

## roster (binary) — **mostly do-at-keyboard**

The event loop that ties it together: read PTY output → advance the emulator → run detection → update core → repaint. Owns key/mouse input, the refresh cadence, and the actual pane-switch side effects. Also hosts the session server (`_server`) — a headless process owning the PTYs of a persistent session, speaking `roster-proto` over a unix socket — and the `_proxy` stdio bridge that carries the same protocol over ssh.

- **Why keyboard:** this is where the async plumbing, timing, and real terminals meet. The loop's correctness is a live-system property. Agents can draft it; you verify and debug it at the keyboard.

---

## The one boundary that matters most

`roster-detect` and `roster-tui` consume the `Grid` model type from `roster-core`, **not** from `roster-pty`, `roster-term`, or live processes. Keep that boundary clean and the four agent-safe crates (`roster-core`, `roster-detect`, `roster-proto`, `roster-tui`) stay fully testable and safe to hand to agents. Break it — let detection reach for a PTY directly — and you lose the entire agent-safe/keyboard split. Guard it.
