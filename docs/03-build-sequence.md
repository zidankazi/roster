# 03 — build sequence

*The order roster was built in, and — per milestone — what's safe to hand agents while you're away versus what needs you at the keyboard. **Milestones 0–5 and persistence are built; this doc is kept for the layering rationale and the agent-safe/keyboard split, which still govern new work.** Build bottom-up: you can't detect state until you can parse a screen, and you can't parse a screen until you can spawn a PTY.*

## Milestone 0 — prove the plumbing (keyboard)

Get one agent running in one PTY, its output parsed into a grid, dumped to stdout. No UI, no sidebar.

- `roster-pty`: spawn `claude` (or any shell) in a PTY, read its bytes. **keyboard**
- `roster-term`: feed those bytes to `alacritty_terminal`, print the resulting grid as text every 500ms. **keyboard**
- **Done when:** you can watch a live agent's screen reconstructed from your own grid. This proves the two hard crates work together. Do this first, at the keyboard — everything depends on it.

## Milestone 1 — the model (agent-safe)

Build the multiplexer's brain with no I/O.

- `roster-core`: pane tree, split, focus, layout math, per-pane metadata. **agent-safe**
- Full unit tests for layout and tree ops.
- **Hand to agents:** yes, entirely. Point Claude Code at `roster-core` with the crate doc + "make `layout()` correct for nested splits, tests must pass." Verifiable without you.

## Milestone 2 — detection (agent-safe, the differentiator)

The heart. Build against `Grid` fixtures, not live agents.

- `roster-detect`: agent identification, the four-state classifier, reason extraction, `agents.toml` loading, debouncing. **agent-safe**
- Capture real screens from Claude Code into fixtures; label each with expected `StateReading`; write the test suite as the contract.
- **Hand to agents:** yes — this is the single best crate for unattended work. Tight spec (`02-state-detection.md`), tests are the contract, no live process. Have agents expand fixture coverage and tune patterns to green.
- **You verify:** spot-check that the tuned patterns match reality on your own live agents; fixtures can drift from real behavior.

## Milestone 3 — the sidebar + panes (agent-safe rendering)

Make it visible and sexy. This is where your UX strength lives.

- `roster-tui`: blit each pane's grid into its rect; render the sidebar (color + label + reason + age, triaged by `roster_core::attention` — blocked/done up, working at the bottom). **agent-safe**
- Snapshot-test the sidebar against known models.
- **Hand to agents:** the rendering and layout, yes. But **you own the taste pass** — the sidebar is the demo, and the demo is the product. Agents get it functional; you make it beautiful.

## Milestone 4 — wire the loop (keyboard)

Tie it together into a running thing.

- `roster` binary: the event loop (PTY output → term → detect → core → tui), input handling, refresh cadence, jump-to-pane side effects. **keyboard**
- **Why keyboard:** async plumbing + timing + live terminals. Agents can draft the loop from the data-flow in `00-architecture.md`; you debug it live. This is where the "looks done but isn't" bugs surface.

## Milestone 5 — ship v1 (done)

- Single-command install: static binary, a Homebrew formula, one curl line. `brew install zidankazi/roster/roster`.
- README with the killer asciinema/gif: several Claude Code agents, glance, jump to the blocked one. **Budget real time for this — the demo is the pitch.**
- Tune Claude Code detection until the sidebar clears the bar in `02`.

## What came after v1

Persistence (detach/reattach via a background server owning the PTYs, attachable over ssh) shipped — the biggest keyboard-side lift, the daemon/IPC layer in `roster-proto` plus the session server. **Next up is the Claude-native attention layer** ([`05-claude-native-attention.md`](05-claude-native-attention.md)): reading Claude Code's hooks and statusline for exact state, an attention inbox, and answering permission prompts from the sidebar.

## The unattended-run recipe

When you step away and want agents working:

1. Point Claude Code at **one** of `roster-core`, `roster-detect`, or `roster-tui` — never at `roster-pty`, `roster-term`, or the binary loop.
2. Give it the crate's doc + "all tests must pass; add tests for any new behavior."
3. Run it sandboxed with auto-accept on (callback: OS-level sandbox so it can work for hours without stalling on prompts, safely).
4. Review the diff in a batch when you're back. You'll feel the review-desk pain firsthand — note it; it's your next project.

The whole crate split exists to make this recipe safe. Respect the agent-safe/keyboard tags and you can genuinely build much of this while you sleep. Ignore them and you'll come back to a broken emulator you can't debug from a diff.
