# 05 — the direction: an engineering-native Claude cockpit

*The strategic direction for roster after persistence. Read this to understand **what we are building next and why it is the differentiator**. Everything in docs 00–04 describes the multiplexer; this describes the bet that makes the multiplexer worth choosing. If you are an agent picking up work on roster, this is the north star — align changes to it.*

The direction has two pillars, built in order:

1. **The Claude-native attention layer** — read Claude Code's own structured
   state (hooks, statusline) instead of scraping pixels, and spend that signal
   on a UI ranked by who needs you and why. Most of this doc specifies it; it
   is in build (Phases 1–4 below).
2. **The instance model** — a workspace stops being "a window with panes" and
   becomes an **instance**: one agent, its branch, its work product. View the
   agent's diff, push its branch, judge its work — without leaving roster.
   Direction committed, design pass pending; see
   [Pillar 2](#pillar-2--the-instance-model-engineering-native) below.

Pillar 1 answers *who needs me and for what*. Pillar 2 answers *what did they
make and is it good*. Together: glance → judge → ship, in one tool.

## Why this is the differentiator: read Claude's state, don't scrape pixels

roster is the cockpit for Claude Code fleets, and that focus *is* the moat. A generic multiplexer has to detect every agent the same way — **regex screen-scraping the terminal grid** (roster's own baseline is [`roster-detect`](../crates/roster-detect), see [02-state-detection.md](02-state-detection.md)). Scraping is a fine baseline, but it has a hard ceiling that a breadth-first, any-agent tool can never leave:

- It can only report **what is painted on screen**. Token burn, context-window remaining, cost, model, rate-limit status, *which* tool is being invoked with *what* arguments — none of that is reliably on the grid, so a scraper can't show it.
- It is **fragile and laggy** — a Claude Code UI tweak breaks a regex; "blocked" surfaces two frames late; "done" is an 8-second recency guess.

Because roster commits to one agent, it can walk through a door a generic tool can't: read Claude Code's own structured state instead of the pixels. That is the whole plan — not a faster status dot, a categorically better signal.

## The bet, in one line

> **Generic tools scrape the pixels of any agent. roster reads Claude Code's actual state — and builds the entire UI around who needs you and why.**

Two moves, fused into one product:

1. **Claude-native depth** — for Claude Code panes, stop reading pixels. Read Claude Code's own structured state: its hooks (exact events + the permission-decision channel) and its statusline feed (telemetry). Get exact events and data the screen never shows.
2. **The attention layer** — spend that richer signal on a UI organized around one question: *which agent needs me right now, and for what?* Not a grid of panes with status dots — an attention queue.

Depth is the fuel; the attention layer is the engine. Neither is interesting alone. Together they are something a breadth-first, any-agent tool structurally cannot copy without abandoning the very generality that defines it.

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

**A hook can decide** (confirmed mechanism — this is what makes answer-from-sidebar real): exit 0 and print a JSON decision on stdout; exit 2 blocks with stderr shown to the user. The envelope is **event-specific** (verified against Claude Code 2.x, ≥ 2.1.119): a **`PermissionRequest`** hook — the one that fires at the permission dialog and can suppress it — carries `hookSpecificOutput.decision.behavior` of `"allow" | "deny" | "ask"`; the `permissionDecision` field belongs to **`PreToolUse`**, which fires *before* the permission check and cannot suppress the prompt. So a roster-owned decision must ride `PermissionRequest`, not `PreToolUse`. roster's auto-approve (Phase 4) prints exactly `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`; that one envelope lives in `crates/roster/src/hook.rs::approve_json`, the single place to re-pin if a version bumps.

> ⚠️ **Still version-dependent — pin at build time.** The event set, exact field names, the settings schema, and the decision contract all move across Claude Code releases (the docs say so explicitly). The specifics above hold for Claude Code 2.x as documented; verify against the installed version, exactly as [02-state-detection.md](02-state-detection.md) pins scraping to Claude Code 2.1. Treat the hook payload as a contract that can drift, not a constant.

### Statusline — the telemetry feed (use this, not the transcript)

Claude Code's custom **statusline** command is fed a rich session JSON on stdin — the sanctioned, documented source for everything the grid never shows. Confirmed fields include `model.id` / `model.display_name`; `cost.total_cost_usd`, `cost.total_duration_ms`, `cost.total_lines_added` / `_removed`; `context_window.context_window_size`, `context_window.used_percentage`, **`context_window.remaining_percentage`**, and `exceeds_200k_tokens`; `rate_limits.five_hour` / `seven_day` (used % + reset time); plus `session_id`, `transcript_path`, `workspace.*`, `pr.*`, `worktree.*`, and `version`.

That is the entire telemetry payload for the attention layer, handed to us structured — **context-remaining is a provided field, not something we compute from token math.** roster's statusline command (`roster _statusline`, registered via `--print-statusline`) forwards this JSON verbatim over the hook socket, routed to the pane by the inherited `ROSTER_PANE` exactly like hook events.

> ⚠️ **Do NOT parse the session transcript `.jsonl` directly.** Claude Code writes a per-session JSONL transcript at `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, and the `transcript_path` on every hook payload points at it — but the **official docs explicitly warn its record format is internal and changes between releases**, so a direct parser breaks on any update. Use `transcript_path` only as a **stable identifier** (which file ⇒ which pane) and as an input to `/export` or the Agent SDK if we ever need turn-level history. Telemetry comes from statusline; events come from hooks. Neither requires reading the raw transcript.

### Terminal title (OSC) — best-effort, already relied on

roster's sidebar card titles already consume the terminal title Claude Code broadcasts (README: "Claude Code sets it to what it's working on"; see the `app` title handling — a card's first line prefers the live task over the config name). Note the honest caveat: **Claude Code setting the title is undocumented** — the official docs describe statusline OSC 8 hyperlinks but not title-setting. So it works empirically today but is not a contract. Keep using it for row labels as a *nice-to-have*, and never let a core signal depend on it — statusline's `workspace` / `session_name` fields are the documented fallback.

### Permission mode

Claude Code's mode — `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` — changes what "blocked" even means: an `acceptEdits` agent will not stop for edit approvals; a `bypassPermissions` one will not stop at all. It is **confirmed observable**: `permission_mode` is on every hook payload. Surface it — an agent in `bypassPermissions` that goes quiet is a *different* situation from one in `default`, and the attention layer should say so.

## Data model: `StateReading` grows, scraping stays as fallback

Do **not** rip out screen-scraping. It is the current detection for Claude panes and the correct baseline before the hook bridge is wired (and for any non-Claude command a user runs in a pane). Make detection **multi-source** and let the richest available source win per pane — hook/statusline data supersedes the scrape when present.

- Extend `StateReading` ([detector.rs](../crates/roster-detect/src/detector.rs)) with optional structured fields, all `Option`, all defaulting to `None` so scraping-only paths are unaffected. Sourced from hooks (future): `tool: Option<String>`, `tool_input: Option<String>`, `permission_mode: Option<Mode>`, `blocked_since: Option<Instant>`. Sourced from statusline — **implemented**: one nested `telemetry: Option<Telemetry>` (the shared `roster_core::Telemetry` type: `model`, `context_pct` from the provided `remaining_percentage`, `cost_usd`, `rate_limit` carrying both documented windows, `five_hour` and `seven_day`), so detection, ranking, and rendering agree on one vocabulary; attached per pane by the `PaneTracker` ([track.rs](../crates/roster-detect/src/track.rs)) from the freshest statusline payload. Do not invent fields with no confirmed source — the transcript is off-limits (see the warning above), so anything not in a hook or statusline payload does not exist for us yet.
- Introduce a **signal-source** notion: a Claude pane prefers the hook/statusline source; every other pane, and any Claude pane with no live bridge data, falls back to the existing classifier. The debouncer in [track.rs](../crates/roster-detect/src/track.rs) still guards scraped signals; **hook events are authoritative and bypass debouncing** (a `Notification` is not a noisy frame — it is a fact).

## Architecture: where it lives

Add the integration as a new bounded piece, respecting the one-way dependency rule in [01-crates.md](01-crates.md):

- A new **`roster-claude`** crate (or, if it stays small, a module in `roster-detect`) owns: installing/locating the bridge, and turning inbound hook events + statusline payloads into structured readings. It depends on `roster-core` (for `AgentState`) and nothing higher. It never reads the raw transcript.
- The **bridge** is a tiny command Claude Code invokes — as a hook on each event (`roster _hook`), and as the statusline command (`roster _statusline`) — that forwards the payload over the existing local socket ([`roster-proto`](../crates/roster-proto)). This reuses the sockets that already exist — do not open a second daemon. Routing is by the pane's inherited `ROSTER_PANE`, not payload matching (see Phase 1).
- Installation must be **opt-in and reversible**, merging into `~/.claude/settings.json` (`hooks` + `statusLine`) without clobbering the user's existing config — namespaced, and with clean removal. Note `statusLine` is singular: if the user already has a statusline, we cannot just add ours; either wrap/chain theirs or make telemetry hook-only for that user. A user who never enables the bridge loses nothing; roster keeps scraping.
- Rendering lives in [`roster-tui`](../crates/roster-tui): the richer cards and the attention queue.

## The attention layer (the UI the depth pays for)

This is what the user actually sees. The organizing principle: **the sidebar is not a list of panes, it is a ranked list of demands on your attention.**

- **Attention inbox** — one queue across every workspace: agents that need you, ranked (longest-blocked first, or destructive-action-pending first), each row the *verbatim* ask from `PreToolUse`/`Notification` ("approve `git push --force origin main`?"), not a scraped fragment. A key jumps to and answers the top item.
- **Answer from the sidebar** — because the hook tells us it is a decision on a known tool, offer allow/deny inline without focusing the pane. The mechanism is confirmed: our `PermissionRequest`/`PreToolUse` hook returns `hookSpecificOutput.permissionDecision` (`allow`/`deny`/`ask`/`defer`) reflecting the user's sidebar answer. This is the single feature a screen-scraper structurally cannot do — it can only send keystrokes to a pane; we can *answer the actual permission request*.
- **Real push, not just reattach** — the usual remote story for these tools is *reattach from your phone*; ours can *push the actual event* the moment it fires ("claude-code blocked: approve deleting `target/`?"). The hook is the trigger.
- **Fleet telemetry** — per card: model, context-remaining %, $ spent, blocked-for duration. Session-wide: total spend, count-waiting, and a **context-exhaustion warning** (from statusline `context_window.remaining_percentage` / `exceeds_200k_tokens`, and the `PreCompact` hook) *before* an agent compacts mid-task and loses the thread.
- **Precise done** — `Stop` retires an agent from the "working" set exactly, so 🔵 done means done, not "quiet for 8 seconds."

## Build sequence

Phased, in the style of [03-build-sequence.md](03-build-sequence.md), tagged **agent-safe** (spec-tight, testable from fixtures, hand it to an agent) vs **keyboard** (needs a human against live Claude Code).

**Phase 1 — the hook bridge, one event end-to-end. Implemented and fixture-tested; live keyboard verification pending.**
Implemented as: roster spawns every pane with `ROSTER_PANE` (its pane id) and `ROSTER_HOOK_SOCK` in the environment; Claude Code hooks inherit that environment (checked empirically against the installed Claude Code — the docs don't promise it), so a `roster _hook` command registered for `PermissionRequest`/`PreToolUse`/`Stop` (via `roster --print-hooks`, merged into `~/.claude/settings.json`; the snippet embeds this binary's absolute path) reports the exact ask the instant it fires. `PermissionRequest` pins the pane 🔴 blocked with the verbatim tool + input (`Bash: cargo test`). Clears are **tool-matched**: an approved tool's `PreToolUse` clears only its own ask (a parallel auto-approved tool can't erase a pending one), `Stop` clears anything (end of turn), and subagent events never clear. Two `roster-proto` frames carry it: `HookBlocked`/`HookClear`, each with the tool. In plain `roster` the app owns a per-process socket at `/tmp/roster-<uid>/hook/<pid>.sock` — a subdirectory, deliberately outside the session-socket namespace `ls`/`kill`/`attach` probe; in `-s` sessions agents report to the session socket and the server relays to the attached client — detection lives client-side either way, and pins apply only to identified claude panes. The pin outranks the scrape on freshness, **not forever**: the scrape keeps running underneath, and once a pin is past a 2s paint grace, a committed non-blocked scrape drops it — a missed clear (interrupt at the prompt fires no hook; a denial fires none until `Stop`) self-heals within ~1s instead of pinning a wrong 🔴 indefinitely. The bridge never fails a Claude session (silent no-op outside roster, always exits 0, stdin read is deadlined). End-to-end plumbing test: `hook_bridge_pins_blocked_and_clears_end_to_end` in `crates/roster/tests/smoke.rs` — it fakes the hook payloads, so it proves roster's side only; **verifying the payload contract against live Claude Code is the open keyboard step before this phase counts as done.**

**Phase 2 — structured fields + statusline telemetry (mostly agent-safe). Statusline parsing, telemetry precedence, the statusline bridge, and the sidebar badge line implemented; payload contract live-verified against Claude Code 2.1.202; hook-sourced fields pending.**
Grow `StateReading`, parse hook + statusline payloads into it. The parsing and the multi-source precedence logic are **agent-safe** — fixture the JSON payloads exactly as detection is fixtured from grids today ([02](02-state-detection.md)), and the tests are the contract. Implemented so far: `statusline::parse` (payload JSON → the shared `Telemetry`, fixture-tested), readings that carry it — `StateReading.telemetry`, attached by the `PaneTracker` with the aging rule under "Open questions" below — and the **bridge itself**: `roster --print-statusline` prints the `statusLine` snippet to merge into `~/.claude/settings.json` (absolute binary path, manual merge exactly like `--print-hooks`, warning on stderr when a statusline is already configured — see the resolved slot-conflict question below), and `roster _statusline` forwards the stdin session JSON **verbatim** as a `Statusline` frame (`roster-proto` tag 14, fire-and-forget — unlike an ask, it never gets a reply) over the same hook socket, under `_hook`'s exact safety contract (silent no-op outside roster, always exits 0, stdin read is deadlined, prints nothing — stdout would become the pane's visible statusline). Parsing stays client-side in `roster-detect`, the one pinned contract point; in `-s` sessions the server relays the frame verbatim like hook clears. The app routes it to the pane — identified, live agent panes only — into `PaneTracker::set_telemetry`, and every detection tick carries the reading's telemetry into the session model (`Session::set_telemetry`, with `None` clearing on staleness) and onto the card: an entry with telemetry grows a third line of badges (`telemetry_line` — model, context % in the severity color, cost, the most-used rate-limit window), while a bridge-less card keeps its exact two-line shape. End-to-end proof: `statusline_telemetry_reaches_the_sidebar_card` in `crates/roster/tests/smoke.rs` — a fake claude pipes the payload through `roster _statusline` and the badge line appears on the real sidebar. **The payload contract is live-verified against Claude Code 2.1.202**: a real claude pane inside roster produced the badge line end-to-end, and the captured payloads (now the fixtures in `roster-detect/tests/fixtures/statusline/`) match every documented field — percentages arrive as integers, and the payload carries additive fields the parser deliberately ignores (`prompt_id`, `effort.level`, `thinking.enabled`, `context_window.current_usage.*`, `workspace.repo.*`). Hook-sourced fields (`tool`, `permission_mode`, …) are not grown yet. (No transcript parsing — see the warning above.)

**Phase 3 — the attention inbox (keyboard, taste).**
The ranked cross-workspace queue in `roster-tui`, verbatim reasons, jump-to-answer. This is a UI-taste milestone; it needs eyes on real fleets. The ordering primitive already exists and the sidebar orders by it today: `roster_core::attention` — blocked first (destructive asks above plain ones, then longest wait), then done, then idle, with working at the bottom, because a working agent needs nothing from the human. The sidebar is that ranking, at exactly one scope: one flat list ranked globally across all workspaces, each card tagged `⧉N` with its home — the cross-workspace ranked view, in-sidebar. (A per-workspace grouping with a `by space · by need` switcher shipped and was deliberately removed: the global ranking is the product's one triage judgment, and offering a second scope diluted it.) The remaining inbox work is the dedicated surface (verbatim reasons, jump-to-answer) on top of the same ranking: one triage judgment, owned in one place, so the sidebar and the inbox can never disagree about who needs you first.

**Phase 4 — answer-from-sidebar, push, telemetry (keyboard). Decision channel + per-pane auto-approve implemented; live-verification pending.**
The **decision channel is wired**: `roster _hook` now waits, deadlined, for a reply on the hook connection, and whoever owns the socket answers with a `HookDecision` frame — the local app from an `Arc<Mutex<HashSet>>` of auto-approved panes (the listener replies inline, before forwarding, and poison-tolerantly), the session server from its own set (told by the client via `SetAutoApprove`, replied *before* the client relay so a slow client can't stall it). The first consumer is a **per-pane auto-approve toggle**, surfaced as a clickable `auto` chip on every card's detail row (muted off, accent on; keyboard: `ctrl-b j`, then `a`): roster answers that pane's `PermissionRequest`s with `allow` while staying observable — the card keeps its real scraped state plus the lit chip, and an auto-approved ask is deliberately *not* pinned 🔴 (the paint-grace would otherwise hold a false blocked against a prompt that never renders). Two semantics are load-bearing and intentional: the toggle is **forward-looking** (it cannot retro-answer an in-flight prompt — only the pane's next asks), and because `HookBlocked` carries no `agent_id` it **necessarily approves the pane's subagent asks too** (pane-level trust). Still ahead on this channel: interactive per-ask allow/deny buttons, push notifications, per-card/session telemetry. **Live-verifying the approve envelope against installed Claude Code is the open keyboard step**, exactly as for Phase 1's payload contract. Each is additive on the Phase 1–2 bridge.

Phase 1 alone already makes roster visibly deliver the one thing we claim as our wedge (showing *why*, exactly and instantly), beyond anything a screen-scraper can reach. Everything after stacks on that bridge.

## Future ideas — not yet scheduled

Raw material for a later phase, surfaced while re-checking this doc against the current Claude Code hook set. None of these are committed or sequenced — they need their own design pass (and a re-verify against the installed Claude Code version) before they become a build-sequence phase. Listed so they aren't lost, not because they're next.

- **Persistent "always allow" from the sidebar.** The answer-from-sidebar mechanism above only covers a one-time allow/deny. The permission-decision response can apparently also carry an `approvalRules` entry (e.g. `Bash: "git push *"`), which would let a sidebar answer install a standing rule so the same prompt never resurfaces. Bigger than an allow/deny toggle — needs thought on where that rule gets written and how it's surfaced as reversible.
- **Safety-net override in `acceptEdits`/`bypassPermissions` mode.** In an auto-accept mode, no permission prompt fires at all, so today's plan has no visibility into a destructive command going through unchecked. `PreToolUse` fires regardless of mode, so roster could flag (not necessarily block) a destructive pattern even when the pane itself shows nothing. The "what counts as destructive" opinion is bounded: it lives as a fixed, reviewed rule table in `roster-detect/src/destructive.rs` (`is_destructive(tool_name, tool_input)` — the input is the *extracted* command string, never the JSON envelope), feeding the `destructive` flag of `roster_core::attention::rank`. Deliberately not user-configurable data for now — extending it is a code change with tests; revisit configurability only if real fleets need tuning. The mode-override UI itself is still unscheduled.
- **Subagent fan-out visibility.** `SubagentStart`/`SubagentStop` are already listed as a secondary signal, but there's a dedicated `subagentStatusLine` surface with a `tasks[]` array (name, status, token count, cwd per subagent) that could turn "one busy pane" into "3 of 5 helper agents done, running: security-review, tests."
- **Task-list progress.** Revises the open question below marked resolved: assume this is buildable, not blocked.
- **PR review state on cards.** Statusline exposes `pr.number` / `pr.review_state` (draft/approved/changes_requested) when a pane's worktree is tied to a PR. Could answer "which of my agents' work is actually mergeable" without leaving roster.
- **Stop-failure reason taxonomy.** `StopFailure` apparently has matchers per failure type (`rate_limit`, `overloaded`, `billing_error`, `authentication_failed`, `server_error`, …). Worth surfacing distinctly per type rather than one generic "failed" state — especially useful for spotting "several agents just hit the same rate limit and went silent" as a fleet-wide event, not five separate mysteries.

## Pillar 2 — the instance model (engineering-native)

*Direction committed 2026-07; no feature below is designed or scheduled yet.
Each needs its own design pass before it becomes a build-sequence phase. This
section exists so agents build **toward** it and stop treating "no diff/review
UI" as a standing non-goal — that non-goal is deliberately reversed.*

The observation: a person running a Claude fleet doesn't just need to know
*who is blocked* — after an agent finishes, they need to judge the work and
ship it. Today that loop leaves roster (a separate terminal for `git diff`,
`git push`, PR creation). The instance model brings it in-product:

> **A workspace is an instance: one agent, its branch, its work product.**

What that unlocks, roughly in dependency order:

- **Per-workspace diff view** — see what the agent changed against its base,
  in-product, without focusing the pane or leaving roster. The statusline feed
  already carries sanctioned inputs (`worktree.*`, `cost.total_lines_added` /
  `_removed`); the diff itself comes from git, not from any Claude surface.
- **Ship actions** — push the instance's branch, open/submit the PR, from the
  workspace. `pr.number` / `pr.review_state` on the statusline feed (see
  "Future ideas") close the loop: which instances are mergeable, at a glance.
- **The UI signal pass** — with workspaces carrying real identity (task,
  branch, diff stat), the chrome gets re-judged for signal-over-noise: every
  visible element must earn its place. This is the same taste milestone as
  Phase 3, widened.
- **Claude-only, staged** — shells stop being a tenant roster offers, in
  three deliberate steps, each gated on the previous one landing:
  1. *Shells can't own a workspace* (implemented): the launcher stops offering
     a `shell` row, and the bare-start placeholder is always replaced by the
     first launch. Shells survive only as splits beside an agent.
  2. *Verification moves in-product*: the diff view and ship actions above —
     the reasons a shell-next-to-the-agent exists today.
  3. *Shells removed entirely*: once step 2 covers the verify loop, panes run
     configured agents only. **Do not jump to step 3 early** — removing
     shells before in-product verification exists trades a real workflow for
     nothing. The "any command runs in a pane" README claim is retired in the
     same change as step 3, not before.

What pillar 2 is **not**: it is not a wrapper. The agent keeps running in a
real PTY pane — full TUI, real keys, scrollback. Structured agent-session
managers put the agent behind a preview widget; roster's terminal fidelity is
the moat, and the instance chrome (diff, ship) wraps *around* the real pane,
never replaces it. And it is still a human watching agents: the
orchestration-API non-goal below survives pillar 2 unchanged.

## Non-goals and honesty

- **Not an agent-orchestration API.** Letting agents drive the multiplexer — spawn helpers, split panes — over a socket is a different product: agents watching agents. Ours is the opposite: a human watching agents. Do not drift into building that. If we ever want an API, it is a separate, later decision.
- **Do not break the screen-based path.** Every change here is additive behind `Option`/fallback. A user who declines the hook install — or runs a non-Claude command in a pane — must see exactly today's screen-based behavior.
- **The moat is only as deep as we go.** A shallow "we have a Claude integration too" is not a moat — anyone can ship one. The defensibility is specifically the **permission-decision loop + statusline telemetry wired into an attention UI** — answering the actual permission request and ranking a fleet by who needs you, not just mirroring a status dot. That is Claude-specific depth a breadth-first, any-agent design will not chase. If we stop at a shallow status echo, we have differentiated nothing.
- **This narrows the story.** "The Claude Code cockpit" is a smaller target than "multiplex any agent." That is the intended trade. State it plainly in positioning rather than straddling.
- **One non-goal was reversed, on purpose.** Earlier versions of docs/00 and
  this doc listed "no git worktrees, no diff/review UI" as deliberate scope
  cuts. Pillar 2 reverses that (decided 2026-07): diff/review/ship UI is now
  committed direction. The reversal is recorded here so no agent "fixes" the
  docs back toward the old scope. The orchestration-API non-goal is **not**
  reversed and never was in question.

## The bar

A person running six Claude Code agents glances at roster and, without touching a pane, knows: who is blocked and on exactly what, who is about to do something they should stop, who is burning context, and who actually finished. A plain status tool can show them six colored dots. That gap is pillar 1.

Then, for the agent that finished: they open its diff, judge it, and push its branch — without leaving roster. That closing of the loop is pillar 2, and the two together are the product.

## Open questions (resolve before/while building)

*Resolved during design (Claude Code 2.x, see Sources): the hook event set, the common payload fields, `PreToolUse`'s `tool_name`/`tool_input`, the `permissionDecision` contract, `permission_mode` on every payload, and the statusline field list. `context_window.remaining_percentage` is provided, so there is no formula to derive. Do not re-litigate these; re-verify them only against a newer Claude Code version.*

Still open:

- The exact **`Notification` payload** fields for notification data (matcher types are known; the data shape was unverified). Confirm before relying on `Notification` over `PermissionRequest`.
- **Statusline slot conflict** — *resolved (v1)*: registration stays a manual merge (`--print-statusline` prints the snippet; roster never edits `~/.claude/settings.json` itself), and when a `statusLine` is already configured the command warns on stderr that merging replaces it — the user decides. Chaining/wrapping the existing command (pipe the same JSON through it, echo its output) is a possible follow-up, deliberately not built: subprocess handling inside the deadline adds failure modes to a path that must never disturb a Claude session.
- **Todo / turn-history source** — previously assumed impossible without the transcript. That assumption needs revisiting: task-lifecycle hook events may now provide a sanctioned, transcript-free source for step-level progress (see "Future ideas" above). Not yet verified against the installed Claude Code version or scoped as a phase.
- **Version pinning**: which minimum Claude Code version to target (e.g. `prompt_id` needs ≥ 2.1.196), and how the bridge degrades on older/newer ones. The statusline payload has been observed live on 2.1.202; all fields the parser maps were present there.
- **Bridge-install UX**: opt-in flow, namespaced merge into `~/.claude/settings.json` (`hooks` + `statusLine`), and clean uninstall.
- **Multi-source precedence**: statusline refresh cadence vs. instantaneous hook events — define which wins for a field. (Phase 1 settled the state-signal case: hook wins on freshness, screen wins on settled reality. Phase 2 settled telemetry aging: the freshest statusline payload rides every committed reading — never debounced, the scrape has no telemetry to offer — and drops back to `None` after `TELEMETRY_STALE_AFTER`, 30s in track.rs, with no newer payload; retune that constant against live cadence once the feed is wired. Per-field arbitration only arises when a second source for a field exists — today statusline is the sole writer.)
- **Protocol version skew**: `roster-proto` has no version handshake, so an old client attached to a new server errors on the first relayed hook frame (unknown tag) and disconnects. Acceptable while releases are 0.0.x and client+server ship in one binary. The auto-approve reply (`HookDecision`, tag 12) inherits this with no new hang: `roster _hook` waits for a reply only within `REPLY_DEADLINE`, so an older peer (or one that predates the channel) that never sends one degrades to today's behavior — Claude shows its own prompt — plus a bounded sub-second stall. The reply is best-effort by design, never a blocked turn. The statusline frame (tag 14) inherits the same skew and makes it bite faster in practice: hook frames only cross on permission events, but with the feed registered a new server relays tag 14 every statusline refresh while any claude works, so a pre-tag-14 client is dropped within seconds of attaching — the fix is the same as ever at 0.0.x, attach with a matching binary. Still needs a capability story before any stability promise.

## Sources

Integration surface confirmed against the official Claude Code 2.x docs:

- Hooks (events, common payload, decision contract): <https://code.claude.com/docs/en/hooks.md>
- Sessions / transcript storage + the "format is internal, don't parse" warning: <https://code.claude.com/docs/en/sessions.md>
- Statusline JSON fields: <https://code.claude.com/docs/en/statusline.md>
- Permission modes: <https://code.claude.com/docs/en/permission-modes.md>
