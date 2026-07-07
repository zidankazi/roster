# 02 — state detection

*The heart of the product and the differentiator. This is where the effort and the taste go. Everything here lives in `roster-detect` and is fully testable from `Grid` fixtures — no live process required. This doc covers screen-based detection — how roster reads Claude Code's state today; the committed direction is to read Claude Code's own state (hooks, statusline) instead, see [`05-claude-native-attention.md`](05-claude-native-attention.md).*

## The state model

Four states, defined by what they mean for the human:

| State | Meaning to you | Detected from |
|---|---|---|
| 🔴 blocked | It needs you now | A prompt/approval/question awaiting input is on screen |
| 🟡 working | It's making progress | Output actively changing; spinner / "esc to interrupt" indicators |
| 🔵 done | It just finished — go look | Idle prompt showing *after* recent completion; output settled |
| 🟢 idle | At rest, nothing pending | Idle prompt, no recent task activity |

**The done-vs-idle split is the subtle, high-value part.** "Done" means *go look now*; "idle" means *ignore*. The signal that separates them is recency: a pane that reached its idle prompt within the last N seconds after a burst of activity is `done`; one that's been sitting at the prompt is `idle`. If this proves hard in v1, collapse them and ship — but do it as a deliberate, documented decision, not an accident.

## Two things to extract, always

For every agent pane, detection produces a `StateReading { state, reason }`:

- **state** — one of the four above.
- **reason** — a short human string. For `blocked`, it's the actual question ("Allow edit to src/config.ts?"). For `working`, a hint ("running tests…"). For `done`, a summary if available ("finished — 3 files changed"). The reason is the whole reason `roster` beats a bare dot; never skip it.

## How classification works

`detector.classify(agent, grid, history)` runs per refresh (~300–500ms):

1. Take the current `Grid` (parsed screen) from `roster-term`.
2. Apply the agent's patterns (from config) against the visible rows — bottom rows first, since prompts live at the bottom.
3. Pick the highest-priority match: `blocked` > `working` > `done` > `idle`. A blocked prompt on screen always wins.
4. Extract the reason from the matched region (e.g. the captured prompt line).
5. Pass the raw reading to the debouncer (below) before it becomes the committed state.

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
done.after_activity_secs = 8      # idle prompt within 8s of activity => done
```

roster ships exactly one agent — Claude Code — tuned to a mirror shine. Depth on Claude Code, not breadth across fifteen flaky agents, is the whole product (docs/05). The deeper move, reading Claude Code's own hooks and statusline instead of screen-scraping, is [`05-claude-native-attention.md`](05-claude-native-attention.md).

## Testing (why this is agent-safe)

Capture real agent screens into fixture files (a `Grid` serialized, or raw text you can build a grid from). Each fixture is labeled with its expected `StateReading`. The test suite feeds fixtures through `classify` and asserts. No PTY, no subprocess, fully deterministic. **This is the ideal crate to hand to Claude Code while you're away:** the spec is tight, the tests are the contract, and correctness is verifiable without you watching. Point it here first.

## The bar

The sidebar is never wrong for more than a second, and someone watching your screen who's never seen the tool can tell which agent needs you and why — without you saying a word. Detection quality *is* the product; spend accordingly.
