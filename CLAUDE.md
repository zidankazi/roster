# CLAUDE.md — working agreement for agents on roster

roster is an agent-aware terminal multiplexer in Rust: it runs coding-agent CLIs
in real panes and shows, in a sidebar, which one is 🔴 blocked / 🟡 working /
🔵 done / 🟢 idle **and why**. Read [`docs/00-architecture.md`](docs/00-architecture.md)
first (the map); [`docs/05-claude-native-attention.md`](docs/05-claude-native-attention.md)
is the committed direction for new work.

This file is the standing contract. It exists because AI-driven development
compounds into slop — added-not-edited code, casual dependencies, dead code left
"just in case", ballooning line counts — unless the rules are explicit and
enforced. Follow it every session.

## Golden rules (the anti-slop contract)

1. **Reuse before you add.** Search for an existing type/function/module before
   writing a new one. Editing the right place beats adding a new place. A new
   file or helper needs a reason.
2. **No new dependency without justifying it in the PR.** A dependency is
   permanent transitive line-count and a supply-chain surface. CI enforces
   licenses/advisories/duplicates (`cargo deny`) and rejects unused deps
   (`cargo machete`). Prefer the std library and what's already in the tree.
3. **Respect the one-way crate graph.** The allowed edges are documented in
   [`docs/01-crates.md`](docs/01-crates.md) and enforced by
   [`scripts/check-arch.sh`](scripts/check-arch.sh). Never add an upward or
   lateral crate edge without updating the allowlist in that script **and**
   saying why in the PR. The graph is the architecture; keep it honest.
4. **One self-contained change per branch, kept small.** No human reads the
   diff — an independent `/code-review` pass is the review, and it's only
   meaningful on a small change.
5. **Change code, update the doc in the same change.** `docs/` is the source of
   truth for architecture and direction. Stale docs are slop too.
6. **Delete dead code.** Don't leave commented-out blocks or unused items
   "in case." Git remembers; the working tree should only hold what's live.
7. **Keep the wedge sharp.** roster's differentiator is state **+ reason**, and
   the Claude-native attention layer (docs/05). Do not drift toward herdr's
   agent-orchestration socket API — that is an explicit non-goal (docs/05).
8. **Never make a gate pass by weakening it.** Do not delete, skip, or loosen
   tests, add `#[allow]` to silence clippy, or widen the deny/arch allowlists
   just to go green. With no human reviewing, the gates are the safety net —
   a gate that legitimately can't pass is a signal to stop and ask, not an
   obstacle to remove.

## Definition of done — all must pass before proposing a merge

- `cargo fmt --all` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — no warnings
- `cargo test --workspace` — green
- `cargo deny check` — advisories/licenses/sources clean
- `cargo machete` — no unused dependencies
- `./scripts/check-arch.sh` — crate graph within the allowlist
- public items documented (`missing_docs` is a workspace lint, denied in CI)
- docs updated if behavior or architecture changed

Run the whole set locally before you claim it works. "Compiles" is not "done".

## Commands

```sh
cargo build                 # build the workspace
cargo test --workspace      # all tests (detection is fixture-tested, no PTY needed)
cargo run -p roster         # run the multiplexer
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-arch.sh     # crate dependency-graph check
```

## Conventions

- **Commits:** brief, all-lowercase `type: subject` — `fix:`, `feature:`,
  `docs:`, `chore:`, `refactor:`, `ci:`. Do **not** add Claude/AI/Anthropic
  attribution or co-author trailers to commits or PR descriptions.
- **Versioning:** never bump, tag, or release a version unless explicitly asked.
  Releases are `0.0.x`, patch-only.
- **Agent detection is data, not code:** new agents are added as entries in
  [`crates/roster-detect/agents.toml`](crates/roster-detect/agents.toml), not by
  patching the classifier. See [`docs/02-state-detection.md`](docs/02-state-detection.md).
- **Two toolchains, isolated:** Cargo owns `crates/`, pnpm owns `website/`.
  Never let one see the other (`docs/00`, `docs/04`).

## What is safe to fully delegate vs needs a human

Per [`docs/00-architecture.md`](docs/00-architecture.md): `roster-core`,
`roster-detect`, and `roster-tui` are **agent-safe** (well-specified, fixture-
testable, no nondeterministic terminal plumbing). `roster-pty`, `roster-term`,
and the wiring in `roster` are **do-at-keyboard** — "looks like it works" and
"actually works" diverge there, and they can't be verified from tests alone.
