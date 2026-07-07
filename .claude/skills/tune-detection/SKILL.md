---
name: tune-detection
description: The full detection-tuning loop for roster — capture live Claude Code screens into fixtures, tune agents.toml patterns, and prove state+reason with tests. Use when the sidebar misreads Claude Code (wrong state, wrong/ugly reason, flicker), when a Claude Code update changed the UI, or when asked to add detection coverage or fixtures.
---

# tune-detection — fixtures from reality, patterns as data

Detection quality IS the product: the bar (docs/02) is a sidebar that is
never wrong for more than a second. Everything here happens in
`crates/roster-detect` + `agents.toml`, and every claim is proven by a
fixture test. Two absolutes:

- **Never hand-write a fixture.** Fixtures are captured from a live Claude
  Code session. If you cannot run one from here, stop and ask the user to
  capture (give them the exact command) — an imagined screen tunes patterns
  against fiction.
- **Never special-case Claude in `detector.rs`.** Patterns, reason sources,
  and ignore rules are data in `agents.toml`. The classifier changes only
  for a new mechanism any agent entry could use — and that is a design
  change to propose, not sneak in.

## The state model you are tuning (docs/02)

`blocked` > `working` > `idle` in match priority; `done` is not a pattern —
it's an idle prompt within `done.after_activity_secs` of activity. The
reason is half the product: `blocked` wants the verbatim ask, `working` the
current activity line, with `reason.ignore` stripping UI chrome (status
bars, interrupt hints) so the reason reads as content.

**The asymmetry that matters:** transitions into blocked commit after ONE
reading (a real "needs you" must surface fast), so a blocked pattern that
ever matches a working screen produces false 🔴 flickers with no debounce
cushion. False-blocked is the worst failure class — always test working and
idle screens against the blocked patterns.

## Step 1 — reproduce with the existing fixtures

```sh
cargo test -p roster-detect
```

If a reported misread isn't covered, that's the gap. Read the current
patterns (`crates/roster-detect/agents.toml`) and the fixture set
(`crates/roster-detect/tests/fixtures/claude-code/`) before touching
anything.

## Step 2 — capture the real screen

The capture harness runs a command in a PTY and prints the parsed grid at
sample times:

```sh
cargo run -p roster --example capture -- "claude" <cols> <rows> <secs,...> [sec:text ...]
```

- `sec:text` sends input at that second — `\r` is enter, `\e` is escape.
- Each sample prints the grid between `--- t=Ns ---` / `--- end t=Ns ---`;
  the child is killed after the last sample.
- 100×30 matches the existing fixtures.

**Safety/cost:** this spawns the user's real, logged-in `claude` (it costs
tokens and can act on the cwd if approved). Run it from a throwaway
directory so prompts reference harmless paths, via `--manifest-path`:

```sh
REPO="$(git rev-parse --show-toplevel)"
mkdir -p /tmp/capture-arena && cd /tmp/capture-arena
cargo run --manifest-path "$REPO/Cargo.toml" -p roster --example capture -- "claude" 100 30 ...
```

Do not send approval keystrokes unless the scenario requires post-approval
screens; capturing the *ask* needs no approval.

Recipes per state (timings vary — sample generously, pick the frame that
shows the state):

| Target | Command sketch |
|---|---|
| idle | `… "claude" 100 30 4` — just the resting prompt |
| working | `… "claude" 100 30 5,8,11 2:'count slowly from 1 to 40\r'` |
| blocked | `… "claude" 100 30 8,12,16 2:'create a file named probe.txt using bash\r'` — the permission dialog is the money frame |
| done | `… "claude" 100 30 6,9,12 2:'say hi\r'` — the settled screen after output stops (the same fixture serves done AND idle tests; classification differs by history, not screen) |

## Step 3 — fixture it

Copy the chosen sample's grid lines **verbatim** (between the markers,
markers excluded) into
`crates/roster-detect/tests/fixtures/claude-code/<state>_<scenario>.txt`.

- Keep blank lines — `Grid::from_text` preserves them, and faithful spacing
  is what makes fixtures honest.
- Name like the existing set: `blocked_allow_edit.txt`,
  `working_esc_hint.txt`, `blocked_wins_over_working.txt`.
- If a Claude Code UI change obsoleted an old fixture, **replace it with a
  fresh capture and say so in the report** — never delete a fixture to make
  the suite pass.

## Step 4 — write the failing test first

In `crates/roster-detect/tests/classify_fixtures.rs`, using the existing
helpers (`classify_fresh`, `classify_after_activity`, `assert_reading`).
Assert **state AND reason** — a state-only assertion tests half the product:

```rust
#[test]
fn claude_blocked_on_bash_permission() {
    assert_reading(
        classify_fresh("claude-code", "claude", "blocked_bash_command.txt"),
        AgentState::Blocked,
        Some("Do you want to proceed?"),
    );
}
```

Run it; watch it fail for the right reason before tuning.

## Step 5 — tune `agents.toml`, minimally

- Patterns are regex over visible rows, matched bottom-up; within a state,
  **earlier patterns win the match and supply the reason** — order is
  meaningful, so place specific patterns before general ones.
- A capture group narrows the reason (`'Allow (.*)\?'` → reason is the
  group).
- New UI chrome polluting `working`/`done` reasons → extend
  `reason.ignore`, don't contort the state patterns.
- Prefer tightening an existing pattern over adding a new one; every
  pattern is a false-positive surface.
- Update the header comment's provenance (which Claude Code version, how
  captured) in the same edit.

## Step 6 — prove it, then guard the flanks

```sh
cargo test -p roster-detect   # new test green, ALL existing fixtures still green
cargo test --workspace        # nothing else regressed
```

Then explicitly check the failure classes the suite encodes:

- every `working_*`/`idle_*`/`done_*` fixture still classifies non-blocked
  (false-blocked check);
- `blocked_wins_over_working.txt` still holds (priority);
- the lifecycle test (`pane_tracker_full_lifecycle`) still passes
  (debounce/timing);
- reasons read as **text, not UI slices** — no box-drawing, no status-bar
  fragments.

If the Claude Code version this was tuned against changed, update the README
line "Detection is tuned against **Claude Code X.Y**" and the `agents.toml`
header together.

## Step 7 — finish

Run `/preflight`. In the report, list: fixtures added/replaced (and the
Claude Code version they were captured from), patterns changed and why, and
anything you could not capture live (→ **not verified**, with the exact
capture command for the user to run).
