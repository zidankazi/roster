---
name: preflight
description: Run roster's full definition-of-done gate suite (fmt, clippy, tests, deny, machete, arch check, metrics delta, docs pass) and produce the standardized landing report. Use when a change is complete, before proposing a merge or running /land, or when asked to "run the gates", "preflight", or "is this done".
---

# preflight — the definition-of-done, executed

roster has no human review: these gates plus `/code-review` ARE the review.
This skill runs every gate, fixes only what is mechanically safe to fix, and
ends with a report the user can trust without reading the diff. It never
commits, never lands, never touches versions.

## Step 0 — situate

```sh
git branch --show-current && git status --porcelain
```

- On `main` with changes → stop: work belongs on a topic branch. Offer to
  create one (`git switch -c <type>/<slug>`) carrying the changes.
- Note uncommitted files; they are part of what you're gating.

## Step 1 — run the gates, in this order

Run each; capture the real output. Do not stop at the first failure — collect
all results so the report is complete (exception: if `cargo build` itself
fails, fix that first; nothing downstream is meaningful).

```sh
cargo fmt --all
```
This one **writes**. Afterwards run `git status --porcelain` — if fmt touched
files *your change didn't touch*, revert those specific files
(`git checkout -- <file>`) so the diff stays single-concern; if it reformatted
your own files, keep it and note "fmt reformatted N files" in the report.

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo machete
./scripts/check-arch.sh
./scripts/metrics.sh --base main
```

Tool availability: `cargo deny`/`cargo machete` missing → report the gate as
**NOT RUN (tool missing)** and suggest `cargo install cargo-deny cargo-machete`.
`metrics.sh` needs `jq`. A gate that didn't run is never reported as passed.

## Step 2 — failure protocol

Fix the **root cause**, then re-run the failed gate and everything after it.
What each failure means, and the forbidden shortcut:

| Gate fails | Fix the cause | NEVER |
|---|---|---|
| clippy warning | restructure the code | `#[allow(…)]` |
| `missing_docs` | write the doc comment (what it IS + the gotcha) | make the item private just to dodge, unless it genuinely shouldn't be public |
| test red | your change is wrong, or the test's expectation legitimately changed — decide which by reading the test's intent | delete/`#[ignore]`/loosen the assertion |
| deny (license/advisory) | drop or replace the dependency | add to the allowlist |
| machete (unused dep) | remove it from Cargo.toml | fake a use |
| check-arch violation | restructure so the edge isn't needed | widen `allowed_for()` |
| flaky-looking test | run it 3×; if genuinely flaky, find the missing deadline/isolation (smoke tests: per-test dirs, `drain_while` deadlines) | retry until green and claim pass |

If a gate **cannot** pass without weakening it, stop: report the exact
failure verbatim and why an honest fix is out of reach. That report is the
deliverable; an amber stop beats a dishonest green.

## Step 3 — docs-consistency pass

List the behaviors your change added/altered. For each, check the owning doc
and README:

- state detection → `docs/02` + `agents.toml` header comment
- crate boundaries/new public surface → `docs/01`
- direction/hook-bridge behavior → `docs/05`
- anything user-visible → `README.md`

`rg` for the old behavior's key terms to catch stale sentences. Update in
this same change.

## Step 4 — the report

End with exactly this shape (it is what the user reads instead of the diff):

```markdown
## preflight — <branch>

**change:** <2-3 sentences: what and why>

| gate | result |
|---|---|
| cargo fmt --all | clean / reformatted N files |
| clippy -D warnings | pass / FAIL: <first error, verbatim> |
| cargo test --workspace | pass (N tests) / FAIL: <test name + assertion> |
| cargo deny check | pass / FAIL / NOT RUN (tool missing) |
| cargo machete | pass / … |
| check-arch.sh | pass / … |

**Δ vs main:** <the metrics.sh line, e.g. "+142 rust loc · +0 transitive deps">
**docs:** <files updated, or "none needed — no behavior change">
**not verified:** <keyboard steps with how-to-check, or "none — fully covered by fixtures/tests">
**next:** /code-review, then /land
```

Rules for the report: paste real output for failures, never paraphrase a
pass you didn't see, and put anything unverifiable-from-here (live PTY
behavior, real Claude Code payloads) under **not verified** with concrete
instructions for checking it at the keyboard.
