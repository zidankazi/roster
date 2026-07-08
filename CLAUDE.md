# CLAUDE.md — operating manual for agents on roster

roster is a terminal multiplexer for Claude Code, written in Rust: it runs
Claude Code in real PTY panes and shows, in a sidebar, which agent is 🔴
blocked / 🟡 working / 🔵 done / 🟢 idle **and why** — the exact prompt it is
waiting on. State **plus reason** is the wedge. Reading Claude Code's own
hooks and statusline instead of scraping pixels is the committed direction.

Read [`docs/00-architecture.md`](docs/00-architecture.md) first — it is the
map. Before any feature work, read
[`docs/05-claude-native-attention.md`](docs/05-claude-native-attention.md) —
it is the north star; changes are judged by whether they sharpen it.

## The contract: how work happens here

**No human reads your diff.** This repo is driven entirely through agents;
the maintainer reviews nothing by eye. The automated gates plus an
independent `/code-review` pass are the only things between your change and
`main`. Three consequences:

1. **You own correctness end to end.** "It compiles" is not "it works", and
   "tests pass" is not "verified" in the keyboard crates. Your final report
   states exactly what you verified, how, and what you did not.
2. **The gates are the safety net.** A gate that legitimately cannot pass is
   a signal to stop and ask — never an obstacle to remove (mistake #1 below).
3. **One small, self-contained change per branch.** Review quality collapses
   with diff size, and here the review is the only defense. CI posts the
   LOC/dependency delta of every PR; growth is a number that gets judged.

Several agent sessions often run in parallel git worktrees against this repo
at once, and `main` moves while you work. The git protocol below (and the
`/land` skill) exists so parallel sessions don't trample each other.

## Map: what lives where, what you may touch

| Crate | Role | Tag |
|---|---|---|
| `roster-core` | pane/window model, layout math, `Grid` type, session snapshot. Zero deps, no I/O. | **agent-safe** |
| `roster-detect` | agent identification, state+reason classification, `agents.toml`, debounce. | **agent-safe** |
| `roster-proto` | framed wire protocol for persistent sessions. Zero deps, hand-rolled. | **agent-safe** |
| `roster-tui` | ratatui rendering: panes, sidebar, launcher, dialogs, theming. | **agent-safe** |
| `roster-pty` | PTY allocation, child spawn (via `portable-pty`). | **do-at-keyboard** |
| `roster-term` | bytes → screen grid (via `alacritty_terminal`). | **do-at-keyboard** |
| `roster` (bin) | event loop, session server, hook bridge, CLI. | **mostly keyboard** |

Agent-safe crates are fully verifiable from fixtures and unit tests — work
there freely. Keyboard crates are where "looks like it works" and "actually
works" diverge; changes there follow the stricter bar in the quality section
and are *labeled* as unverified-live in your report.

The dependency graph is one-way, bottom-up, and **enforced** by
[`scripts/check-arch.sh`](scripts/check-arch.sh). The boundary that matters
most: detection and rendering consume the `Grid` type from `roster-core`,
never anything from `roster-term`/`roster-pty`/live processes. That boundary
is what keeps them testable and safe to hand to agents.

**Two toolchains, isolated.** Cargo owns `crates/`; the JS package manager
whose lockfile sits in `website/` owns `website/` — never let one see the
other, never add a second lockfile. The only permitted bridge is a build-time
JSON artifact emitted by the Rust side. `docs/` is plain markdown read by
agents, not a rendered site.

Docs index: `00` map · `01` crates+boundaries · `02` state detection ·
`03` build order + delegation rationale · `04` website · `05` direction.
`docs/` is the source of truth; stale docs are a bug you fix in the same
change that made them stale.

## The loop: every task, start to finish

1. **Branch off the current `main`.** Never commit on `main`; never reuse
   another session's branch. Topic branches are `feat/<slug>`, `fix/<slug>`,
   etc.
2. **Read before writing.** The owning module, its tests, and the doc that
   covers it. Search (`rg`) for existing types/helpers before adding any.
3. **Make the smallest change that solves the task.** Edit the owning place;
   a new file, type, or helper needs a reason you can state in one sentence.
4. **Run the gates** — all of them, listed under "Quality bar". The
   `/preflight` skill runs the suite and writes the report.
5. **Run `/code-review`.** Correctness and security findings are blocking:
   fix them, or state in the report exactly why a finding is wrong — never
   ignore one silently.
6. **Report** (format under "When uncertain"). State what you did *not*
   verify as plainly as what you did.
7. **Land** via the `/land` skill (or hand the branch off if landing is
   unsafe). Never race other sessions for `main`.

## House style

Match the existing code exactly — it is deliberately uniform. The specifics:

### Rust

- **Every public item has a `///` doc comment** — `missing_docs` is a
  workspace lint and CI runs with `-D warnings`, so an undocumented `pub`
  item fails the build. The voice is a noun phrase or short sentence stating
  what the thing *is*, plus the gotcha if there is one:
  `/// A history with no recorded frames.` ·
  `/// The reason is always updated; last_change moves only when the state
  actually changes value.` Struct fields and enum variants included.
- **Module docs (`//!`)** open every file: purpose, invariants, and a pointer
  to the owning doc (`See docs/02-state-detection.md`).
- **Comments explain why, invariants, and trade-offs — never narrate code.**
  The tree's best comments record timing subtleties, security reasoning, and
  deliberate compromises ("the hook wins on freshness and richness, the
  screen wins on reality"). If a comment restates what the next line does,
  delete it.
- **Errors:** `roster-core` returns `Option`/`bool` (callers decide);
  libraries hand-roll small error enums with `Display` + `Error` impls
  (`ConfigError`, `PtyError`) or use `io::Result` (`roster-proto`); the
  binary uses `Result<T, String>` for user-facing failures. **No `anyhow`,
  no `thiserror`** — don't introduce them.
- **Panics:** never in library code paths. `expect()` only for genuine
  invariants, with a message naming the invariant
  (`.expect("embedded agents.toml is valid")`). In the binary's runtime
  loop, I/O failure degrades (mark dead, keep looping) and poisoned locks
  recover: `.unwrap_or_else(|p| p.into_inner())`.
- **Dependencies:** std first, then what's already in the tree.
  `roster-core` and `roster-proto` stay **zero-dependency**. A new external
  dependency is a stop-and-ask decision, not a convenience.
- **Config parsing fails loudly.** `deny_unknown_fields`, errors that name
  the agent and the offending value — a typo must error, never silently
  not-match.
- **Naming:** types are exact nouns (`PaneRuntime`, `HookPin`); functions are
  `verb_noun` (`drain_output`, `sync_layout`); constants SCREAMING_SNAKE with
  a doc comment, placed next to their use, not in a constants file.
- **Formats that persist or cross a socket are compatibility surfaces.**
  `roster-proto` frame tags are append-only; the session snapshot is
  versioned (`v1`) — extend by adding, keep old readers working, or bump the
  version and handle both.

### Tests

- Unit tests live inline, `#[cfg(test)] mod tests` at the bottom of the file;
  integration tests in `crates/<crate>/tests/`.
- **Names are behavioral sentences**: `blocked_commits_on_first_reading`,
  `earlier_pattern_outranks_lower_row`. If the name doesn't state an
  expectation, rename it.
- Detection is **fixture-tested**: real captured screens in
  `crates/roster-detect/tests/fixtures/`, each asserted for state **and
  reason**. No PTY in detection tests, ever.
- Render tests construct a `View`, draw to a ratatui `TestBackend`, and
  assert on buffer cells/regions — including *style* assertions when style is
  the point. No golden files.
- Smoke tests (`crates/roster/tests/smoke.rs`) run the real binary in a PTY
  against **fake agent scripts** on `PATH`, with a deadline on every wait and
  per-test scratch dirs (concurrent tests on Linux hit ETXTBSY on a shared
  script). Match on grid text, never on raw bytes or timing.
- A bug fix ships with the test that would have caught it.

### Commits and branches

- Message format: `type: subject` — all lowercase, imperative, no trailing
  period. Types: `fix`, `feat` (**never** `feature:`), `docs`, `chore`,
  `refactor`, `ci`. A body is welcome on behavior-changing commits: explain
  the why; reference prior commits by hash when relevant.
- **No AI attribution anywhere** — no `Co-Authored-By: Claude`, no
  "generated with" footers, in commits, PRs, or issues. Omit them entirely.
- **Never bump, tag, or release a version unless explicitly asked.**
  Releases are `0.0.x`, patch-only, and follow the release checklist below
  only on request.

### The multi-agent git protocol

Worktrees isolate files, **not** the shared `main` ref. The failure mode is
real: N sessions finishing at once and racing to land becomes a rebase-retry
livelock. The rules:

- **One branch per unit of work; never two sessions on the same feature.**
  If you discover a branch/worktree already implementing your task, stop and
  ask.
- **You push only your own branch.** Never push, force-push, or rewrite
  `main` or anyone else's branch.
- **Landing is serialized** through the `/land` protocol: rebase onto the
  current `main`, re-run the tests, then fast-forward `main` only — either
  `git merge --ff-only` from a clean `main` checkout, or a compare-and-swap
  `git update-ref refs/heads/main <new> <old>` where `<old>` is the tip your
  branch is rebased onto (your first commit's parent). A guard that isn't
  your parent can silently drop another session's commits.
- **The primary checkout stays on clean `main`** as the reference tree;
  sessions work in their own worktrees.
- Two failed landing attempts means contention — stop and report instead of
  looping.

### Detection is data, not code

Screen-detection behavior lives in
[`crates/roster-detect/agents.toml`](crates/roster-detect/agents.toml):
regex patterns per state, reason sources, ignore rules. The classifier
(`detector.rs`) changes only for new *mechanisms* available to any agent
entry — never for a Claude-specific quirk. Patterns are tuned against
fixtures captured from live Claude Code (see `/tune-detection`), and the
provenance comment in `agents.toml` plus the README's "tuned against Claude
Code X.Y" line move together with any retune. roster ships exactly one agent
— Claude Code — tuned deeply; breadth across agents is explicitly not the
product.

### The Claude Code integration surface

- Events come from **hooks**, telemetry from the **statusline** feed.
  **Never parse the session transcript `.jsonl`** — its format is
  documented-unstable; `transcript_path` is only an identifier.
- Hook/statusline payloads are **version-dependent contracts**. The approve
  envelope lives in one place (`crates/roster/src/hook.rs::approve_json`) so
  a Claude Code version bump has one re-pin point. If observed payloads
  disagree with docs/05, report the drift — don't silently adapt.
- Everything hook-driven is **additive behind `Option`/fallback**: a user
  without the bridge, or a non-Claude command in a pane, gets exactly the
  screen-scraped behavior. The bridge never fails a Claude session (silent
  no-op outside roster, always exit 0, deadlined reads).
- **Non-goal, permanently:** an agent-orchestration socket API (agents
  driving the multiplexer). roster is a human watching agents, not agents
  watching agents. Do not drift toward it.

## The mistakes a weaker model makes here — and the rule that prevents each

1. **Gate-softening.** Deleting/skipping/loosening a failing test, adding
   `#[allow(…)]` to silence clippy or `missing_docs`, widening `deny.toml`
   licenses or the `check-arch.sh` allowlist, relaxing an assertion — to go
   green. → *A gate that can't pass honestly is a stop-and-ask. No
   exceptions; with no human review, a weakened gate protects nothing.*
2. **The helper-file reflex.** Creating `utils.rs`, `helpers.rs`, or a
   parallel module instead of editing the owning one. → *Search first; edit
   in place. A new file/type/helper needs a one-sentence reason in the
   commit body.*
3. **The dependency reflex.** Reaching for `anyhow`/`itertools`/`chrono`/…
   for a one-liner. → *std first; the tree hand-rolls its errors and framing
   on purpose. A new dependency is a proposal to the user, not a decision
   you make. `cargo machete` and `cargo deny` will fail you anyway.*
4. **Patching the classifier for a screen quirk.** Hardcoding Claude Code
   behavior in `detector.rs`. → *Detection changes are `agents.toml` data +
   fixtures. Code changes only for new mechanisms any agent entry could
   use.*
5. **The imagined fixture.** Editing regexes against what Claude Code
   "probably prints", or hand-writing fixture files. → *Every pattern change
   ships with a fixture captured from a live session
   (`cargo run -p roster --example capture`) and a test asserting state AND
   reason.*
6. **Trusting one frame.** Wiring a scraped signal straight into committed
   state, or weakening the debouncer. → *Scraped readings go through
   `Debouncer`; only hook events are authoritative and bypass it. Extra
   danger: blocked commits after ONE reading, so a false-positive blocked
   pattern has no debounce cushion — test working screens against blocked
   patterns explicitly.*
7. **The upward edge.** Adding a crate dependency edge because it compiles
   (detect → term "for real grids"; core → anything). → *The graph in
   `check-arch.sh` IS the architecture. Needing a new edge means stop and
   ask, with the design case.*
8. **"Compiles = works" in keyboard land.** Claiming PTY/emulator/event-loop
   behavior works from tests alone. → *Changes to `roster-pty`,
   `roster-term`, or the binary's runtime loop are labeled
   "needs keyboard verification: <what to check, how>" in your report.
   Never claim live behavior you didn't observe.*
9. **Frame-tag drift.** Reusing or renumbering a `roster-proto` tag, or
   changing an existing frame's encoding. → *Tags are append-only; every
   variant round-trips in `every_frame_round_trips`; corruption inputs
   error, never panic. There is no version handshake — note skew
   consequences.*
10. **Transcript temptation.** Parsing `~/.claude/projects/**/*.jsonl` for
    state or history. → *Off-limits (docs/05). Hooks for events, statusline
    for telemetry, `transcript_path` only as an identifier.*
11. **The while-I'm-here diff.** Drive-by refactors, renames, reformatting
    neighboring code, "cleanups" riding a feature branch. → *One concern per
    branch. Log the discovery in your report as a proposed follow-up
    instead.*
12. **Dead code "just in case".** Commented-out blocks, unused `pub` items,
    parallel legacy paths. → *Delete it; git remembers. The working tree
    holds only what's live.*
13. **The version-bump reflex.** Bumping the workspace version, tagging,
    editing the formula/installer, "preparing a release" helpfully. →
    *Never, unless the user explicitly asked this session.*
14. **Toolchain bleed.** Running JS tooling at the repo root, letting Cargo
    reference `website/`, committing `node_modules`/`.next`, adding a second
    JS lockfile. → *Hard isolation; the only bridge is a build-time JSON
    artifact.*
15. **Git races.** Advancing `main` with a stale guard, force-pushing shared
    branches, working directly on `main`, duplicating a feature another
    session owns, resetting worktrees you don't own. → *Follow the
    multi-agent git protocol; land through `/land`; two failed attempts →
    stop.*
16. **Chrome that disappears.** Styling roster's own UI with
    `Modifier::DIM` or theme-dependent ANSI colors (dark navy on a dark
    terminal). → *roster chrome goes through `style.rs` semantics —
    `state_color()`, `muted()` (fixed Indexed 243), `ACCENT` — and DIM is
    reserved for faithfully rendering guest program output. Two shipped
    fixes exist for exactly this bug, and a regression test guards it —
    extend it, don't fight it.*

## Quality bar per deliverable — checkable, not adjectives

### Every change (the definition of done)

All of these, run locally, actually green — before proposing a merge:

```sh
cargo fmt --all                                        # then: no stray reformat of unrelated files
cargo clippy --workspace --all-targets -- -D warnings  # includes missing_docs on public items
cargo test --workspace
cargo deny check                                       # advisories, licenses, sources
cargo machete                                          # no unused dependencies
./scripts/check-arch.sh                                # crate graph within the allowlist
./scripts/metrics.sh --base main                       # LOC/dep delta — account for growth in the report
```

- [ ] Docs updated in the same change if behavior or architecture moved
- [ ] `/code-review` run; correctness/security findings fixed or rebutted
- [ ] Commit messages follow the convention; no attribution
- [ ] Report written (format below), including what was NOT verified

### Detection change (`agents.toml`, `roster-detect`)

- [ ] Behavior expressed as data in `agents.toml`, not code in `detector.rs`
- [ ] Each new/changed pattern has a live-captured fixture in
      `tests/fixtures/claude-code/`
- [ ] A test in `classify_fixtures.rs` asserts **state and reason** for it
- [ ] All existing fixtures still pass — they are the regression net; a
      fixture obsoleted by a Claude Code UI change is *replaced with a fresh
      capture*, stated in the report, never just deleted
- [ ] Working/idle screens tested against blocked patterns (false-blocked
      has no debounce cushion)
- [ ] Timing behavior (done-vs-idle window, debounce) covered when touched
- [ ] Provenance updated: `agents.toml` header comment + README version line

### TUI change (`roster-tui`)

- [ ] All colors/styles via `style.rs` semantics; no raw ANSI for chrome, no
      DIM outside guest-cell rendering
- [ ] Render test constructs the `View` and asserts cells/regions — style
      assertions included when style is the point
- [ ] Click targets in `hit.rs` updated in the same change as moved chrome,
      with a test
- [ ] No I/O added — the tui emits intents; the binary wires effects
- [ ] Degenerate sizes (tiny panes, zero width) don't panic
- [ ] README updated if user-visible

### Protocol change (`roster-proto`)

- [ ] New capability = new variant with a **new** tag; existing encodings
      byte-identical
- [ ] Encode + decode branches, lengths validated, `MAX_FRAME` respected
- [ ] Round-trip added to `every_frame_round_trips`; corruption inputs error
- [ ] Both endpoints handle it; relay/direction semantics documented on the
      variant (client→server? relayed? never-relayed?)
- [ ] Version-skew consequence noted in docs/05 open questions if user-facing

### Binary / keyboard-crate change (`roster`, `roster-pty`, `roster-term`)

- [ ] Smallest viable change; smoke-testable parts covered in `smoke.rs`
      (fake agent script, deadlines on every wait, per-test dirs)
- [ ] Runtime loop stays panic-free: poison-recovering locks, best-effort
      writes that mark dead rather than crash
- [ ] Socket paths only via `vetted_sessions_dir()`/`socket_in()` — the vet
      is structural; never resolve a socket path around it
- [ ] Pane output handling respects generation tags
- [ ] No platform assumptions outside `cfg(unix)` — CI is macOS + Linux
- [ ] Report labels exactly what needs keyboard verification and how

### Docs change

- [ ] The owning doc updated (00 map / 01 crates / 02 detection / 03 build
      order / 04 website / 05 direction), plus README when user-facing
- [ ] Old claims grepped for and removed — no contradicting sentence left
- [ ] Comparisons stay generic ("tmux", "GUI managers", "status-only
      tools"); never name specific competing products in the repo
- [ ] Links resolve

### Website change (`website/`)

- [ ] Everything under `website/`; Cargo untouched; root `.gitignore` still
      covers `node_modules`/`.next`
- [ ] Use the package manager matching the existing lockfile in `website/` —
      never introduce a second lockfile (if the lockfile and docs/04
      disagree, the lockfile is reality; reconcile the doc in the same
      change)
- [ ] Production build succeeds locally

### Release (ONLY when explicitly asked)

- [ ] Bump workspace `Cargo.toml` version → `cargo check` (refresh lockfile)
      → commit + push
- [ ] Confirm CI on `main` is green first — the tag workflow does **not**
      run tests
- [ ] Tag `v0.0.X`, push the tag; watch `gh run list` for the 4-target build
- [ ] Bump the Homebrew tap (separate repo `zidankazi/homebrew-roster`):
      formula url to the new tag + fresh `sha256` of the source tarball

## When uncertain: escalation rules

**Hard stops — end the turn with a question/report instead of proceeding:**

1. A gate can only pass by weakening it.
2. The change seems to need a new external dependency, a new crate, a new
   crate-graph edge, or a new license in `deny.toml` — propose with the
   justification; don't add it.
3. Anything involving versions, tags, releases, or publishing.
4. Deleting or rewriting work you didn't author this session (files, tests,
   branches, history) beyond the task's obvious scope.
5. Another branch/worktree already implements (or half-implements) your
   task.
6. Two landing attempts failed — `main` is contended.
7. Success depends on live terminal/Claude Code behavior you cannot observe
   from here — ship the fixture-tested part, label the rest, and say what to
   check by hand.
8. Observed Claude Code hook/statusline payloads contradict docs/05 —
   report version drift; don't silently adapt the contract.
9. The task, understood properly, wants to grow past one self-contained
   change — propose the split.

**Resolve yourself — do not ask:**

- Code and docs disagree on a fact → verify empirically (run it), then fix
  both in the same change.
- Two analyses (yours, a reviewer's) disagree on factual behavior → run the
  experiment. A five-line test settles what argument cannot.
- Ambiguity a test can capture → write the test.
- Where code belongs → the module that owns the concern (search first).
- Naming, formatting, comment style → match the file you're in.

**The report — how every task ends.** State plainly: what changed and why
(2-3 sentences); gate results as actually observed (paste failures
verbatim); the metrics delta (`+N loc · +M deps`); docs touched; **what was
not verified** and how a human can verify it; recommended next step. Never
write "done" over an unrun gate, and never claim live behavior from a green
unit test.

## Commands

```sh
cargo build                      # build the workspace
cargo test --workspace           # all tests (detection is fixture-tested, no PTY needed)
cargo run -p roster              # run the multiplexer
cargo run -p roster --example capture -- "claude" 100 30 3,6,10   # capture live screens → fixtures
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                 # needs: cargo install cargo-deny
cargo machete                    # needs: cargo install cargo-machete
./scripts/check-arch.sh          # crate dependency-graph check
./scripts/metrics.sh --base main # size + dependency delta vs main (needs jq)
```

## Skills

- **`/preflight`** — runs the whole definition-of-done suite in order and
  writes the landing report. Use it at the end of every change.
- **`/tune-detection`** — the full detection-tuning loop: capture live
  screens, fixture them, adjust `agents.toml`, prove state+reason. Use it
  whenever the sidebar misreads Claude Code or a Claude Code update shifts
  the UI.
- **`/land`** — the serialized, contention-safe landing protocol for a
  finished branch. Use it instead of touching `main` by hand.
