---
name: orchestrate
description: Lead-agent delegation loop for roster — do recon inline, write one spec-quality brief, hand the implement/test/lint loop to the cheaper `sidekick` subagent, then review the diff and correct by re-briefing rather than rewriting. Use for agent-safe crate work (roster-core, roster-detect/agents.toml, roster-proto, roster-tui) when the task has a delegable implementation. Skip it for keyboard-crate or serial-debugging work where the accumulated context IS the work.
---

# orchestrate — lead delegates, sidekick implements

The premise (from the Fusion "make Fable cheaper than Opus" result): an
agent's cost is dominated by the **lead's** turn count and context size, not by
per-token price. Leads that delegate *early* with a good brief — and review
instead of rewriting — take a third of the turns and drag a third of the
context. This skill is that discipline for roster: you (the lead) stay the
manager; the `sidekick` subagent (cheaper model, roster house-rules baked in)
does the implement/test/lint loop.

## Step 0 — decide if this task is delegable at all

Delegation has no leverage when there is nothing to hand off. **Do NOT use this
skill** — do the work inline — when:

- The task touches `roster-pty`, `roster-term`, or the binary runtime loop.
  Those need live keyboard verification you can't review away (CLAUDE.md
  mistake #8). The lead keeps them.
- It's a serial debugging hunt where the root cause is one long chain of
  judgments — the accumulated context is the work.
- It's a handful of turns with nothing between deciding and shipping.

**Good candidates:** `agents.toml` detection tuning (fixture-tested),
`roster-core` layout/model changes, `roster-proto` frame additions, `roster-tui`
render work — anything verifiable from fixtures/unit/render tests. If the task
is delegable but mixed, split it: delegate the agent-safe part, keep the
keyboard part.

## Step 1 — recon inline, cheaply

Discover the work-list yourself before delegating — this is fast and it's what
makes the brief good. Read the owning module and its tests; `rg` for existing
types/helpers so the brief can say "reuse X" instead of the sidekick inventing
one. For detection work, capture the live screen NOW if the brief will need a
fixture:

```sh
cargo run -p roster --example capture -- "claude" 100 30 3,6,10
```

Do NOT start implementing. The moment you write the design decisions and pull
the important files into your context is the moment the expensive work is
already done — that is the anti-pattern.

## Step 2 — write ONE spec-quality brief

The brief is the whole game. The winning example from the post: the lead wrote
*"operator() must be O(1) in pointer length: NO full token scan"* (score 94);
the lead that hand-coded it forgot the constraint and shipped linear time
(score 25). Specify constraints and outcomes, not keystrokes.

A roster brief states, concretely:

- **The change**, in one or two sentences — the module that owns it, the
  smallest edit that solves it.
- **Constraints** — the house rules this task will trip on (zero-dep crate?
  append-only tag? `style.rs` only, no DIM? `deny_unknown_fields`?). Name them
  so the sidekick doesn't rediscover them by failing a gate.
- **Definition of done, as artifacts** — *which fixture must pass, which gate
  must go green*. "state=blocked, reason='Do you want to proceed?' asserted in
  `classify_fixtures.rs`" beats "make detection work." A gate is a checkable
  definition of done; use it as one.
- **What NOT to do** — the false paths recon revealed (don't add a helper file,
  don't touch `detector.rs`, reuse `Grid::foo`).

Then delegate with the Agent tool, `subagent_type: "sidekick"`. Pass the brief
as the prompt. For purely mechanical work you may override `model: haiku`;
default sidekick (sonnet) for anything with correctness stakes.

## Step 3 — review the diff cheaply, correct by re-briefing

When the sidekick reports back, review at lead prices only where review is
cheap:

```sh
git diff        # or: git show, on what the sidekick changed
```

Read its report's "what I did NOT verify" section — that's where the real risk
is. Then:

- **Trust the gates it ran.** Don't pull its files back into your context and
  re-derive them. The post's Opus-as-micromanager reverted good sidekick work
  and rewrote it by hand for no correctness gain — don't.
- **If review finds a real bug, prefer another cheap handoff** ("your fixture
  asserts state but not reason — add the reason assertion and the pattern for
  it") over a lead-price rewrite. Rewrite yourself only when the fix genuinely
  needs context only you hold.
- **Keyboard-crate claims are yours to verify**, always — if the change grew to
  touch one, that part comes back to you, labeled for hand-verification.

## Step 4 — close out through the existing gates

The sidekick's local gates are not the merge bar. Before proposing a merge you,
the lead, still run `/preflight` and `/code-review`, write the report (what
changed, gate results as observed, metrics delta, what was NOT verified), and
land via `/land`. Delegation changed who typed the code; it did not move the
definition of done.

## One-line honesty check

If you find yourself implementing before you've written a brief, or rewriting
the sidekick's diff instead of re-briefing it, you're the micromanager the post
warns about — stop and hand the judgment down.
