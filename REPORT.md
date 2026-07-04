# Build report

Two unattended runs. Run 1 covered the agent-safe crates (`roster-core`,
`roster-detect`, `roster-tui`). Run 2, on your go-ahead, built the rest:
`roster-term`, `roster-pty`, the `roster` binary, and the install path
(GitHub releases + Homebrew tap).

## Run 2 — plumbing, binary, packaging

**roster-term** (12 tests). `Screen` wraps `alacritty_terminal` 0.26: feed
PTY bytes, snapshot a `roster_core::Grid`. Colors (ANSI/256/truecolor),
attributes, cursor state, wrapping, and — the critical one for full-screen
agents — alternate-screen switch/restore are all covered by tests that
drive real escape sequences through the parser. Scrollback is off by design
(detection and rendering only read the visible screen).

**roster-pty** (8 tests). `Pty::spawn` runs a command line via `sh -c`
inside a `portable-pty` PTY (env inherited, `TERM=xterm-256color`); reader
cloning, input writes, resize (verified via `stty size` from a live child),
exit codes, kill, and drop-kills-the-child are all integration-tested
against real processes.

**roster binary** (16 tests incl. e2e). Event loop wiring output → emulator
→ detection (400ms cadence) → model → repaint (50ms input poll); tmux-style
`ctrl-b` prefix (`%`/`"` split, `o` focus, `x` close, `j` sidebar jump mode,
`q` quit); key encoding to PTY byte sequences (unit-tested); config lookup
at `~/.config/roster/agents.toml` with built-in fallback. The smoke test is
end-to-end and headless: it plants a fake `claude` on `PATH` that prints a
blocked prompt, runs the real binary inside a PTY, parses roster's own
output with `roster-term`, asserts the sidebar shows
`● claude-code blocked: Do y… ⏱`, then quits via prefix-q and checks a
clean exit.

**Packaging.** MIT license; `ci.yml` (fmt + clippy -D warnings + tests on
macOS/Linux — green on GitHub); `release.yml` builds tarballs for
aarch64/x86_64 macOS and Linux on `v*` tags. `v0.1.0` is tagged and
released with all four artifacts + sha256s. The Homebrew tap
`zidankazi/homebrew-roster` has a formula that builds from the tagged
source. **Verified end to end on this machine:**
`brew install zidankazi/roster/roster` poured the rust toolchain, built
roster in 24s, and the installed binary answers `roster 0.1.0`; `brew test
roster` passes.

## Run 1 — agent-safe crates (milestones 1–3)

Covered `roster-core`, `roster-detect`, and `roster-tui`, per
`docs/03-build-sequence.md`.

## What was built

**Workspace.** Cargo workspace with the three agent-safe crates; docs copied
into `docs/`; `.gitignore` covers both toolchains. `missing_docs` is a
workspace lint, so every public item is documented.

**roster-core** (35 tests). `Grid` (cells + styles + cursor, buildable from
text fixtures via `Grid::from_text`), the binary split tree with layout math
(`Rect`, `SplitDirection`, exact tiling with deterministic rounding), and
`Session` (windows, split/close/focus with collapse-and-refocus semantics,
`set_reading` that moves `last_change` only on real state changes).

**roster-detect** (48 tests). `Detector` with `identify` (basename matching
of the pane command) and `classify` (per-frame reading); `agents.toml`
parsing with strict validation (unknown keys, bad regexes, and unknown
reason sources all fail loudly); `History` (content-change fingerprint +
last-activity recency); `Debouncer` (K=2 to commit, K=1 into blocked);
`PaneTracker` bundling the per-pane loop. Signal priority: blocked pattern >
working pattern > content changed since last frame > idle pattern (read as
done within the per-agent recency window) > idle. 16 fixture screens for
Claude Code / Codex / Aider drive the contract suite, including a
full-lifecycle test (idle → working → blocked → working → done → idle) that
asserts the committed state at every frame, debounce lags included.

**roster-tui** (17 tests). `PaneView` (grid → buffer blit with style
mapping, clipping), `Sidebar` (dot + agent + state: reason + right-aligned
age; blocked/done sorted up, longest-waiting first within a state;
truncation with ellipsis; selection highlight), `SidebarState` emitting
`Message::JumpToPane` — the binary owns the side effect. Top-level
`render()` composes panes + sidebar; snapshot-tested through ratatui's
`TestBackend`.

## State

`cargo test --workspace`: 136 passed, 0 failed (100 from run 1 + 36 from
run 2). `cargo fmt --check` and `cargo clippy --all-targets -- -D
warnings`: clean, enforced by CI (green on GitHub for macOS and Linux).
Every commit green and pushed to `main`; `v0.1.0` released with binaries
for four targets; Homebrew install verified locally.

## Decisions you should review

1. **`Grid` lives in `roster-core`, not `roster-term`.** The docs put the
   grid type in `roster-term`, but that crate is keyboard-scoped and out of
   this run. Since detect/tui only *consume* the grid, I defined it in the
   agent-safe world; `roster-term` can wrap `alacritty_terminal` and produce
   this type (or re-export it) when you build it. Wide-character cells and
   escape handling are explicitly deferred to the emulator.
2. **Two shipped patterns adjusted for trimmed rows.** Grid rows are matched
   with trailing whitespace trimmed, so the doc's codex idle pattern
   `'^\S+ ❯ $'` and aider's `'^> $'` became `'^\S+ ❯\s*$'` and `'^>\s*$'`
   (same intent, commented in `agents.toml`).
3. **Reason extraction niceties.** `matched_line` uses the regex's first
   capture group when present, else the whole matched line; reasons are
   stripped of surrounding box-border characters; `last_nonempty` skips
   rows with no alphanumeric characters (pure box-drawing chrome) so a
   working reason is never `╰────╯`. `done` reasons come from the last
   worded line above the idle prompt.
4. **Pattern-order priority.** Within a state, patterns are tried in config
   order, each scanning rows bottom-up. This makes "Do you want to
   proceed?" beat the `❯ 1. Yes` menu row for the blocked reason while both
   still detect.
5. **Unrecognized static screens read as idle.** No pattern match + no
   content change falls through to idle (with debouncing this is the
   conservative choice). A static screen sitting on an *unknown* blocked
   prompt will read idle — the fix is config coverage, not code.

## Deliberately skipped

- `roster-pty`, `roster-term`, the `roster` binary/event loop (keyboard
  scope), and the website (docs say build it last, after v1 works).
- Done-state summary extraction beyond the last-worded-line heuristic.
- Sidebar scrolling for more agents than rows, and the focused-pane visual
  treatment — both are part of the taste pass the docs reserve for you.

## Flag for your keyboard time

- **Fixtures are synthesized, not captured.** I wrote them to be realistic,
  but they encode the doc's patterns, not live screens. Spot-check against
  real Claude Code / Codex / Aider sessions (docs/03 calls this out as your
  verification step). In particular: verify Claude Code's input row really
  renders so that `'│\s*>\s*$'` can match — if the box has a right border,
  the pattern and the idle/done fixtures need adjusting together.
- `identify` matches the pane's direct command only; walking the process
  tree when the command is a shell belongs to the binary (needs OS calls).
- Versions picked by `cargo add` today: regex 1.12, serde 1.0, toml 1.1,
  ratatui 0.30 (the post-0.29 workspace split — API verified against the
  crate source, not memory).
