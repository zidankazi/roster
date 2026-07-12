# 02 — state detection

*The heart of the product and the differentiator. This is where the effort and the taste go. Everything here lives in `roster-detect` and is fully testable from `Grid` fixtures — no live process required. This doc covers screen-based detection — how roster reads Claude Code's state today; the committed direction is to read Claude Code's own state (hooks, statusline) instead, see [`05-claude-native-attention.md`](05-claude-native-attention.md). The crate also hosts the hook-payload destructive-ask predicate (`destructive.rs`) — string classification for the attention inbox, not grid detection.*

## The state model

Four states, defined by what they mean for the human:

| State | Meaning to you | Detected from |
|---|---|---|
| 🔴 blocked | It needs you now | A prompt/approval/question awaiting input is on screen |
| 🟡 working | It's making progress | Output actively changing; spinner / "esc to interrupt" indicators |
| 🔵 done | It just finished — go look | Idle prompt showing *after* recent completion; output settled |
| 🟢 idle | At rest, nothing pending — free for work | Idle prompt, no recent task activity |

**The done-vs-idle split is the subtle, high-value part.** "Done" means *go look now*; "idle" means *ignore — this agent is free*. The signal that separates them is recency: a pane that reached its idle prompt within the last N seconds after a burst of activity is `done`; one that's been sitting at the prompt is `idle`. If this proves hard in v1, collapse them and ship — but do it as a deliberate, documented decision, not an accident.

The N-second window is when the **detector** reports `done`. The app layer then extends it for a pane the human wasn't watching: if a pane turns done while it is *not* the focused pane, roster keeps 🔵 done displayed until the human focuses it — focus is the acknowledgment — rather than letting it expire on the timer while nobody looked. A pane that finished *while focused* keeps the pure timed decay (the human watched it happen). This latch is app/session-layer state (`Pane::unseen`, `Session::mark_seen`), not a detector concern — `roster-detect` is untouched.

## Two things to extract, always

For every agent pane, detection produces a `StateReading { state, reason, telemetry }`:

- **state** — one of the four above.
- **reason** — a short human string. For `blocked`, it's the actual question ("Allow edit to src/config.ts?"). For `working`, a hint ("running tests…"). For `done`, a summary if available ("finished — 3 files changed"). The reason is the whole reason `roster` beats a bare dot; never skip it.
- **telemetry** — bridge-fed numbers (model, context %, cost, rate limit; the `roster-core` `Telemetry` type), only when the pane has a live statusline feed (docs/05). Never scraped: `classify` always leaves it `None`; the `PaneTracker` attaches the freshest payload without debouncing and ages it instead of freezing it: the fast-moving fields (model, context %, cost) drop after 30 quiet seconds, while rate-limit windows with a reported reset keep asserting — their countdown re-derived from the arrival stamp each tick — until the reset itself passes (docs/05). A scraping-only pane behaves exactly as before the field existed.

## How classification works

`detector.classify(agent, grid, history)` runs per refresh (~300–500ms):

1. Take the current `Grid` (parsed screen) from `roster-term`.
2. Apply the agent's patterns (from config) against the visible rows — bottom rows first, since prompts live at the bottom.
3. Pick the highest-priority match: `blocked` > `working` > `done` > `idle`. A blocked prompt on screen always wins.
4. Extract the reason from the matched region (e.g. the captured prompt line).
5. Pass the raw reading to the debouncer (below) before it becomes the committed state.

When no `blocked`/`working` pattern matches, a change in the grid since the last frame still reads as `working` — output is moving even if nothing recognizable is on screen. That change fingerprint deliberately skips blank rows, any row matching the agent's `activity.ignore` patterns, and — when `activity.ignore_region` is set — the composer box: the bottom-most row matching the region's start pattern through the next row matching its end pattern (wrapped continuation rows of a long unsent prompt carry no prompt glyph of their own). Rows *below* the box, like the background-task tray, still count. The composer echoes every keystroke of an *unsent* prompt and status chrome toggles on its own, and none of that is the agent doing work. Without the exclusions, a human typing reads as 🟡 working and stamps fake activity into the done-window bookkeeping.

Two further guards keep a fresh pane's startup chrome out of the done window (the observed failure: a spawned Claude Code paints its banner and prompt, sits quiet, then appends an MCP-authentication notice seconds later — and the pane read 🔵 done without ever being asked anything):

- **The change signal is gated on the screen having settled once** — two consecutive frames with the same *non-blank* fingerprint. Until then, every differing frame is the program painting its initial UI, and matching blank frames prove nothing painted, not that the screen held still. Pattern-matched `working` (the spinner) is not gated.
- **Activity is stamped from the committed state, not the raw frame.** A single changed frame (the late MCP notice; a wrapped composer shifting the transcript one row) reads as raw `working` but never survives the debouncer, so it can never arm the done window. Real work commits `working` within two polls and stamps from then on — so an agent launched straight into a task still reads done when it finishes. The trade-off: work that starts and finishes inside a single poll interval never commits and its completion reads idle, not done; the hook bridge (docs/05) owns that case.

`history` carries the last few readings + timestamps, needed for the done/idle recency call and for debouncing.

## Debouncing — the trust feature

**Never flip a committed state on a single frame.** This is the rule that makes people trust the sidebar. A double state-flip that repaints the dot mid-frame produces false "blocked" flickers — exactly what makes a status tool feel unreliable and get abandoned.

- Require a candidate state to persist for K consecutive readings (start K=2–3) before committing it.
- Exception: transitions *into* `blocked` may commit faster (1 reading), because a real "needs you" should surface quickly and a brief false-blocked is less costly than a missed one. Tune this.
- Rock-solid-but-slightly-laggy beats twitchy. Always.

## Per-agent config: `agents.toml`

The shipped config is Claude Code only. Detection is still **data, not code** — a user can add their own agents in `~/.config/roster/agents.toml` without touching the classifier — but roster ships and tunes exactly one.

```toml
[claude-code]
# how to recognize the pane
match_command = ["claude"]
# patterns evaluated against visible grid rows (regex)
blocked = ['Do you want to proceed\?', 'Allow .*\?', '❯ \d\. Yes']
working = ['esc to interrupt', 'Thinking', '[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]']  # spinner glyphs
idle    = ['│\s*>\s*$']                                     # empty prompt line
# where to pull the human-readable reason from, per state
reason.blocked = "matched_line"   # use the line that matched `blocked`
reason.working = "last_nonempty"  # last non-empty output line
# rows whose changes are not agent activity (composer echo, status chrome)
activity.ignore = ['^\s*❯', '^\s+●', '^─+$']
# the composer box: bottom-most prompt row through its closing border
activity.ignore_region = ['^\s*❯', '^─+$']
done.after_activity_secs = 8      # idle prompt within 8s of activity => done
```

roster ships exactly one agent — Claude Code — tuned to a mirror shine. Depth on Claude Code, not breadth across fifteen flaky agents, is the whole product (docs/05). The deeper move, reading Claude Code's own hooks and statusline instead of screen-scraping, is [`05-claude-native-attention.md`](05-claude-native-attention.md).

## Testing (why this is agent-safe)

Capture real agent screens into fixture files (a `Grid` serialized, or raw text you can build a grid from). Each fixture is labeled with its expected `StateReading`. The test suite feeds fixtures through `classify` and asserts. No PTY, no subprocess, fully deterministic. **This is the ideal crate to hand to Claude Code while you're away:** the spec is tight, the tests are the contract, and correctness is verifiable without you watching. Point it here first.

## The bar

The sidebar is never wrong for more than a second, and someone watching your screen who's never seen the tool can tell which agent needs you and why — without you saying a word. Detection quality *is* the product; spend accordingly.
