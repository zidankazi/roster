---
name: land
description: Safely integrate a finished roster branch into main amid concurrent agent sessions — duplicate-work check, rebase, post-rebase re-verify, then a fast-forward-only land (ff merge from the main checkout, or a parent-guarded compare-and-swap). Use when asked to "land", "merge to main", "integrate", or "ship" a completed branch.
---

# land — serialized, contention-safe integration

Several agent sessions work this repo in parallel worktrees. Worktrees
isolate files, **not** the shared `main` ref — N sessions racing to land
becomes a rebase-retry livelock, and a wrong force can silently drop another
session's commits. This protocol turns the race into a queue. History stays
linear: fast-forward only, no merge commits, no rewrites of shared refs.

## Step 0 — preconditions (all three, no exceptions)

1. `/preflight` ran green on this branch's current tip in this session.
2. `/code-review` ran; correctness/security findings fixed or explicitly
   rebutted in the report.
3. Everything is committed (`git status --porcelain` clean) with
   convention-formatted messages (`type: subject`, lowercase, no
   attribution).

Missing any → do that first. Landing unreviewed work defeats the only
review this repo has.

## Step 1 — survey the field

```sh
git fetch origin 2>/dev/null || true
git worktree list
git branch -a --sort=-committerdate
```

Establish:

- **Where is `main` checked out, and is it clean?**
  (`git -C <path> status --porcelain` for the worktree that has it.)
- **Duplicate work?** Scan recent branch names for another branch
  implementing the same feature (`git log --oneline main..<branch>` on
  suspects). Two branches for one feature is a hard stop — report both and
  ask which survives.
- Which worktree is *yours*. You may reset/rebase only branches you own.
  Never touch another session's worktree or branch, even to "help".

## Step 2 — sync with a possibly-moved main

```sh
git rev-parse main
git merge-base main HEAD
```

- Equal → your branch sits on the current tip; go to step 3.
- Different → `main` moved: `git rebase main`.
  - Conflicts only in files you authored → resolve and continue.
  - Conflicts in files you didn't touch → abort (`git rebase --abort`),
    stop, and report — another session's change collides with yours and a
    human call is needed.
- **After ANY rebase, re-verify before landing** — a clean rebase can still
  break behavior:

```sh
cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Step 3 — land, by exactly one of these paths

Let `BRANCH` be your branch and `BASE=$(git merge-base main HEAD)` (equal to
`main`'s tip after step 2).

**Path A — `main` is checked out in a worktree you control and it is clean:**

```sh
git -C <main-worktree> merge --ff-only "$BRANCH"
```

`--ff-only` refuses anything that isn't a pure fast-forward — if it refuses,
`main` moved again: go back to step 2 (attempt #2).

**Path B — `main` is NOT checked out in any worktree:**

```sh
git update-ref refs/heads/main "$(git rev-parse HEAD)" "$BASE"
```

The third argument is a compare-and-swap guard: the update succeeds only if
`main` still equals `BASE` — the tip your commits descend from — which
structurally guarantees a fast-forward. **Never** use a freshly re-read
`main` as the guard (if `main` moved after your rebase, that guard "passes"
while your tip doesn't contain the new commits — a silent non-ff force that
drops another session's work), and **never** land with `git branch -f main`
or any `--force`.

If the CAS fails, `main` moved: go back to step 2 (attempt #2).

**Path C — neither is safe** (main checked out dirty somewhere, or it's not
yours): don't land. Push your branch and hand off:

```sh
git push origin "$BRANCH"
```

Report that the branch is ready and why landing was unsafe.

## Step 4 — contention brake

**Two failed attempts (refused ff / failed CAS) = stop.** More retries is
the livelock. Push your branch (path C) and report: "main is contended;
branch ready at <sha>, rebases cleanly as of <main-sha>". The user or a
single designated integrator session lands the queue.

## Step 5 — after landing

```sh
git log --oneline main -3   # your subject on top, previous tip directly beneath
```

- Delete the merged topic branch: `git branch -d "$BRANCH"` (`-d`, not
  `-D` — it must already be reachable from main). Leave other sessions'
  branches alone.
- If you operated the primary checkout, leave it on clean `main`.
- **Pushing `origin/main` is not part of landing.** Push it only when the
  user has said to in this session; otherwise report "landed locally, not
  pushed".

## Never

- Push, force-push, rewrite, or `branch -f` `main` or any branch you don't
  own.
- Land a branch whose gates you didn't see pass yourself, this session, on
  the exact tip being landed.
- Resolve another session's conflict by discarding its side.
- Retry the land loop more than twice.
