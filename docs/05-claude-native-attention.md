# 05 — the Claude-native attention layer

*The strategic direction for roster after persistence. Read this to understand **what we are building next and why it is different from herdr**. Everything in docs 00–04 describes the multiplexer; this describes the bet that makes the multiplexer worth choosing. If you are an agent picking up work on roster, this is the north star — align changes to it.*

## The problem this solves: we are not yet different from herdr

Be honest about the competitive picture, because the plan only makes sense against it.

[herdr](https://github.com/ogulcancelik/herdr) is the same product as roster, further along: a single Rust binary, an agent-state sidebar (🔴🟡🔵🟢 — the same four glyphs), persistent detach/reattach sessions, ssh attach, mouse-native with a `ctrl-b` prefix, 14+ agent integrations, a commercial license, and a large head start on adoption. Feature-for-feature we are near-identical, and **we will not win a parity race** on the generic multiplexer. That ground is taken.

We are also tied on *technique*, and that is the opening. Both tools detect agent state the same way: **regex screen-scraping the terminal grid** (ours is [`roster-detect`](../crates/roster-detect), see [02-state-detection.md](02-state-detection.md); herdr calls it "process-name matching + terminal-output heuristics"). Scraping pixels has a hard ceiling:

- It can only report **what is painted on screen**. Token burn, context-window remaining, cost, model, rate-limit status, *which* tool is being invoked with *what* arguments — none of that is reliably on the grid, so neither tool can show it.
- It is **fragile and laggy** — a Claude Code UI tweak breaks a regex; "blocked" surfaces two frames late; "done" is an 8-second recency guess.

Herdr cannot leave that ceiling without abandoning its "works with any of 14 agents" pitch — generic scraping is the only thing that works across all of them. **We can leave it, for one agent.** That is the whole plan.

## The bet, in one line

> **herdr scrapes the pixels of any agent. roster reads Claude Code's actual state — and builds the entire UI around who needs you and why.**

Two moves, fused into one product:

1. **Claude-native depth** — for Claude Code panes, stop reading pixels. Read Claude Code's own structured state: its hooks (exact events + the permission-decision channel) and its statusline feed (telemetry). Get exact events and data the screen never shows.
2. **The attention layer** — spend that richer signal on a UI organized around one question: *which agent needs me right now, and for what?* Not a grid of panes with status dots — an attention queue.

Depth is the fuel; the attention layer is the engine. Neither is interesting alone. Together they are something herdr structurally cannot copy without ceasing to be herdr.

This is a **deliberate narrowing**: roster becomes "the cockpit for running Claude Code fleets" first, and a generic multiplexer second. That is the right trade at this stage. Own it in the README; do not hide it.

## What we read instead of pixels

Claude Code already emits structured state we currently throw away by scraping the rendered TUI. The integration surface, richest-signal first:

### Hooks — the exact-event channel

Claude Code runs user-configured shell commands on lifecycle events, each handed a JSON payload on stdin. This turns a screen-scraped guess into a **deterministic event the instant it happens**, with structured detail.

The events we care about (all confirmed against the Claude Code 2.x hook docs — see Sources):

- **`PermissionRequest`** — fires when the permission dialog appears. This is our cleanest, zero-latency **blocked** signal: an explicit "the human must decide" event, no regex, no debounce, no missed prompt. Prefer it over scraping `Do you want to proceed\?`.
- **`Notification`** — fires when Claude needs attention more broadly (matcher types include `permission_prompt`, `auth_success`, and idle-waiting-for-input). A secondary blocked/attention signal alongside `PermissionRequest`.
- **`PreToolUse`** — fires before a tool runs, carrying **`tool_name`** and **`tool_input`** (e.g. `{ "command": "npm test" }`). This is *what* the agent is about to do — the real command/diff/file behind a permission prompt, not a scraped line — and where a destructive action (`Bash(rm -rf …)`, `git push --force`) is identifiable **before** it happens.
- **`Stop`** — fires when the agent finishes its turn: a **precise "done"**, replacing the 8-second activity-window guess in `done.after_activity_secs`. **`StopFailure`** fires when a turn ends on an API error — surface that distinctly (it is not a clean "done").
- **`PermissionDenied`**, **`PostToolUse`**/**`PostToolUseFailure`**, **`UserPromptSubmit`**, **`SubagentStart`/`SubagentStop`**, **`SessionStart`/`SessionEnd`**, **`PreCompact`/`PostCompact`** — secondary signals: denials, tool results/failures, when the human last spoke, subagent fan-out, session lifecycle, and imminent context compaction (a great early-warning for the attention layer). The full event set is large; take what earns its keep.

**Every hook payload carries** (confirmed common fields): `session_id`, `prompt_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, and optionally `agent_id` / `agent_type` (subagents) and `effort.level`. `PreToolUse` adds `tool_name` + `tool_input`. This is how the session server routes an event to the right pane: match on `session_id` / `cwd`.

**A hook can decide** (confirmed mechanism — this is what makes answer-from-sidebar real): exit 0 and print a JSON decision on stdout; exit 2 blocks with stderr shown to the user. The decision JSON carries `hookSpecificOutput.permissionDecision` of `"allow" | "deny" | "ask" | "defer"` plus `permissionDecisionReason` and an optional `updatedInput`, alongside top-level `continue` / `suppressOutput` / `systemMessage`. A roster-owned `PermissionRequest`/`PreToolUse` hook can therefore hand the decision to the sidebar and return the user's answer.

> ⚠️ **Still version-dependent — pin at build time.** The event set, exact field names, the settings schema, and the decision contract all move across Claude Code releases (the docs say so explicitly). The specifics above hold for Claude Code 2.x as documented; verify against the installed version, exactly as [02-state-detection.md](02-state-detection.md) pins scraping to Claude Code 2.1. Treat the hook payload as a contract that can drift, not a constant.

### Statusline — the telemetry feed (use this, not the transcript)

Claude Code's custom **statusline** command is fed a rich session JSON on stdin — the sanctioned, documented source for everything the grid never shows. Confirmed fields include `model.id` / `model.display_name`; `cost.total_cost_usd`, `cost.total_duration_ms`, `cost.total_lines_added` / `_removed`; `context_window.context_window_size`, `context_window.used_percentage`, **`context_window.remaining_percentage`**, and `exceeds_200k_tokens`; `rate_limits.five_hour` / `seven_day` (used % + reset time); plus `session_id`, `transcript_path`, `workspace.*`, `pr.*`, `worktree.*`, and `version`.

That is the entire telemetry payload for the attention layer, handed to us structured — **context-remaining is a provided field, not something we compute from token math.** Register a roster statusline command (or reuse one) that forwards this JSON to the session server over the socket, keyed by `session_id`.

> ⚠️ **Do NOT parse the session transcript `.jsonl` directly.** Claude Code writes a per-session JSONL transcript at `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, and the `transcript_path` on every hook payload points at it — but the **official docs explicitly warn its record format is internal and changes between releases**, so a direct parser breaks on any update. Use `transcript_path` only as a **stable identifier** (which file ⇒ which pane) and as an input to `/export` or the Agent SDK if we ever need turn-level history. Telemetry comes from statusline; events come from hooks. Neither requires reading the raw transcript.

### Terminal title (OSC) — best-effort, already relied on

roster's workspace auto-naming already consumes the terminal title Claude Code broadcasts (README: "Claude Code sets it to what it's working on"; see the `launcher`/`app` title handling). Note the honest caveat: **Claude Code setting the title is undocumented** — the official docs describe statusline OSC 8 hyperlinks but not title-setting. So it works empirically today but is not a contract. Keep using it for row labels as a *nice-to-have*, and never let a core signal depend on it — statusline's `workspace` / `session_name` fields are the documented fallback.

### Permission mode

Claude Code's mode — `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` — changes what "blocked" even means: an `acceptEdits` agent will not stop for edit approvals; a `bypassPermissions` one will not stop at all. It is **confirmed observable**: `permission_mode` is on every hook payload. Surface it — an agent in `bypassPermissions` that goes quiet is a *different* situation from one in `default`, and the attention layer should say so.

## Data model: `StateReading` grows, scraping stays as fallback

Do **not** rip out screen-scraping. It is the correct fallback for non-Claude agents (Codex, Aider, anything in `agents.toml`) and for Claude panes before the hook bridge is wired. Instead, make detection **multi-source** and let the richest available source win per pane.

- Extend `StateReading` ([detector.rs](../crates/roster-detect/src/detector.rs)) with optional structured fields, all `Option`, all defaulting to `None` so scraping-only paths are unaffected. Sourced from hooks: `tool: Option<String>`, `tool_input: Option<String>`, `permission_mode: Option<Mode>`, `blocked_since: Option<Instant>`. Sourced from statusline: `model: Option<String>`, `context_pct: Option<f32>` (the provided `remaining_percentage`), `cost_usd: Option<f32>`, `rate_limit: Option<RateLimit>`. Do not invent fields with no confirmed source — the transcript is off-limits (see the warning above), so anything not in a hook or statusline payload does not exist for us yet.
- Introduce a **signal-source** notion: a Claude pane prefers the hook/statusline source; every other pane, and any Claude pane with no live bridge data, falls back to the existing classifier. The debouncer in [track.rs](../crates/roster-detect/src/track.rs) still guards scraped signals; **hook events are authoritative and bypass debouncing** (a `Notification` is not a noisy frame — it is a fact).

## Architecture: where it lives

Add the integration as a new bounded piece, respecting the one-way dependency rule in [01-crates.md](01-crates.md):

- A new **`roster-claude`** crate (or, if it stays small, a module in `roster-detect`) owns: installing/locating the bridge, and turning inbound hook events + statusline payloads into structured readings. It depends on `roster-core` (for `AgentState`) and nothing higher. It never reads the raw transcript.
- The **bridge** is a tiny command Claude Code invokes — as a hook on each event, and as the statusline command — that forwards the JSON payload to the roster **session server** over the existing local socket ([`roster-proto`](../crates/roster-proto)). This reuses the persistence server that already exists — do not open a second daemon. The server routes each payload to the pane whose `session_id` / `cwd` matches.
- Installation must be **opt-in and reversible**, merging into `~/.claude/settings.json` (`hooks` + `statusLine`) without clobbering the user's existing config — namespaced, and with clean removal. Note `statusLine` is singular: if the user already has a statusline, we cannot just add ours; either wrap/chain theirs or make telemetry hook-only for that user. A user who never enables the bridge loses nothing; roster keeps scraping.
- Rendering lives in [`roster-tui`](../crates/roster-tui): the richer cards and the attention queue.

## The attention layer (the UI the depth pays for)

This is what the user actually sees. The organizing principle: **the sidebar is not a list of panes, it is a ranked list of demands on your attention.**

- **Attention inbox** — one queue across every workspace: agents that need you, ranked (longest-blocked first, or destructive-action-pending first), each row the *verbatim* ask from `PreToolUse`/`Notification` ("approve `git push --force origin main`?"), not a scraped fragment. A key jumps to and answers the top item.
- **Answer from the sidebar** — because the hook tells us it is a decision on a known tool, offer allow/deny inline without focusing the pane. The mechanism is confirmed: our `PermissionRequest`/`PreToolUse` hook returns `hookSpecificOutput.permissionDecision` (`allow`/`deny`/`ask`/`defer`) reflecting the user's sidebar answer. This is the single most herdr-can't-do feature in the plan — a screen-scraper can only send keystrokes to a pane; we can *answer the actual permission request*.
- **Real push, not just reattach** — herdr's remote story is *reattach from your phone*; ours can *push the actual event* the moment it fires ("claude-code blocked: approve deleting `target/`?"). The hook is the trigger.
- **Fleet telemetry** — per card: model, context-remaining %, $ spent, blocked-for duration. Session-wide: total spend, count-waiting, and a **context-exhaustion warning** (from statusline `context_window.remaining_percentage` / `exceeds_200k_tokens`, and the `PreCompact` hook) *before* an agent compacts mid-task and loses the thread.
- **Precise done** — `Stop` retires an agent from the "working" set exactly, so 🔵 done means done, not "quiet for 8 seconds."

## Build sequence

Phased, in the style of [03-build-sequence.md](03-build-sequence.md), tagged **agent-safe** (spec-tight, testable from fixtures, hand it to an agent) vs **keyboard** (needs a human against live Claude Code).

**Phase 1 — the hook bridge, one event end-to-end (keyboard).**
Wire `PermissionRequest` → hook command → `roster-proto` socket → session server → the matching pane's `StateReading`, replacing the scrape for the blocked signal on Claude panes. The smallest proof the thesis holds: a blocked event that is exact and instant. Do this at the keyboard against live Claude Code; confirm the payload contract for the installed version first.

**Phase 2 — structured fields + statusline telemetry (mostly agent-safe).**
Grow `StateReading`, parse hook + statusline payloads into it. The parsing and the multi-source precedence logic are **agent-safe** — fixture the JSON payloads exactly as detection is fixtured from grids today ([02](02-state-detection.md)), and the tests are the contract. Registering the statusline command and wiring it to live sessions is **keyboard**. (No transcript parsing — see the warning above.)

**Phase 3 — the attention inbox (keyboard, taste).**
The ranked cross-workspace queue in `roster-tui`, verbatim reasons, jump-to-answer. This is a UI-taste milestone; it needs eyes on real fleets.

**Phase 4 — answer-from-sidebar, push, telemetry (keyboard).**
The decision contract, notifications, per-card and session telemetry. Each is additive on the Phase 1–2 bridge.

Phase 1 alone already makes roster visibly better than herdr on the one thing we claim as our wedge (showing *why*, exactly and instantly). Everything after stacks on that bridge.

## Non-goals and honesty

- **Not an agent-orchestration API.** Herdr lets agents drive the multiplexer (spawn helpers, split panes) over a socket. That is *their* bet — agents watching agents. Ours is the opposite: a human watching agents. Do not drift into building their product. If we ever want an API, it is a separate, later decision.
- **Do not break the generic path.** Every change here is additive behind `Option`/fallback. A Codex or Aider user, or a Claude user who declines the hook install, must see exactly today's behavior.
- **The moat is only as deep as we go.** Herdr already ships an npm skill; "we have an integration too" is not a moat. The defensibility is specifically the **permission-decision loop + statusline telemetry wired into an attention UI** — answering the actual permission request and ranking a fleet by who needs you, not just mirroring a status dot. That is the Claude-specific depth herdr's breadth-first design will not chase. If we stop at a shallow status echo, we have differentiated nothing.
- **This narrows the story.** "The Claude Code cockpit" is a smaller target than "multiplex any agent." That is the intended trade. State it plainly in positioning rather than straddling.

## The bar

A person running six Claude Code agents glances at roster and, without touching a pane, knows: who is blocked and on exactly what, who is about to do something they should stop, who is burning context, and who actually finished. Herdr can show them six status dots. That gap is the product.

## Open questions (resolve before/while building)

*Resolved during design (Claude Code 2.x, see Sources): the hook event set, the common payload fields, `PreToolUse`'s `tool_name`/`tool_input`, the `permissionDecision` contract, `permission_mode` on every payload, and the statusline field list. `context_window.remaining_percentage` is provided, so there is no formula to derive. Do not re-litigate these; re-verify them only against a newer Claude Code version.*

Still open:

- The exact **`Notification` payload** fields for notification data (matcher types are known; the data shape was unverified). Confirm before relying on `Notification` over `PermissionRequest`.
- **Statusline slot conflict**: `statusLine` is a single slot. Decide the strategy when a user already has one — chain/wrap theirs, or fall back to hook-only telemetry for that user.
- **No todo / turn-history source** without the transcript. If we ever want the agent's todo list or per-turn history, the sanctioned routes are `/export` or the Agent SDK, not the `.jsonl`. Decide if it is worth that complexity; otherwise the feature does not exist for us.
- **Version pinning**: which minimum Claude Code version to target (e.g. `prompt_id` needs ≥ 2.1.196), and how the bridge degrades on older/newer ones.
- **Bridge-install UX**: opt-in flow, namespaced merge into `~/.claude/settings.json` (`hooks` + `statusLine`), and clean uninstall.
- **Multi-source precedence**: statusline refresh cadence vs. instantaneous hook events — define which wins for a field and how stale telemetry is aged out.

## Sources

Integration surface confirmed against the official Claude Code 2.x docs:

- Hooks (events, common payload, decision contract): <https://code.claude.com/docs/en/hooks.md>
- Sessions / transcript storage + the "format is internal, don't parse" warning: <https://code.claude.com/docs/en/sessions.md>
- Statusline JSON fields: <https://code.claude.com/docs/en/statusline.md>
- Permission modes: <https://code.claude.com/docs/en/permission-modes.md>

Competitive reference: [herdr](https://github.com/ogulcancelik/herdr) · <https://herdr.dev/>
