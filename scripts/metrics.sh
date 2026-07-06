#!/usr/bin/env bash
#
# metrics.sh — report codebase size and dependency footprint.
#
# Usage:
#   metrics.sh                 report on this repo
#   metrics.sh <dir>           report on another checkout (CI uses this for the base tree)
#   metrics.sh --base <ref>    report on this repo + a delta line vs <ref>, computed
#                              locally via a temporary worktree (no PR needed)
#
# Prints a markdown table (per-crate Rust LOC + totals, transitive dependency
# count, workspace crate count, test count) plus a machine-readable HTML-comment
# TOTALS line (invisible when rendered). CI runs this on every PR so growth is a
# number the reviewer approves — see .github/workflows/ci.yml.
#
# Portable to bash 3.2 (macOS default). Needs `cargo` and `jq`.

set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"

# emit <target-repo-root> : markdown table + TOTALS comment for that tree.
emit() {
    local target="$1"
    echo "| crate | rust loc |"
    echo "|---|---:|"
    local total_loc=0 name loc
    for dir in "$target"/crates/*/; do
        [ -d "$dir" ] || continue
        name=$(basename "$dir")
        loc=$(find "$dir" -name '*.rs' -type f -print0 2>/dev/null | xargs -0 cat 2>/dev/null | wc -l | tr -d ' ')
        loc=${loc:-0}
        total_loc=$((total_loc + loc))
        printf '| %s | %s |\n' "$name" "$loc"
    done
    printf '| **total** | **%s** |\n' "$total_loc"

    local pkgs members ext_deps tests
    pkgs=$(cargo metadata --format-version 1 --manifest-path "$target/Cargo.toml" 2>/dev/null | jq '.packages | length')
    members=$(cargo metadata --format-version 1 --manifest-path "$target/Cargo.toml" 2>/dev/null | jq '.workspace_members | length')
    ext_deps=$(( pkgs - members ))
    # `|| true`: grep exits 1 on zero matches, which would abort the script
    # under `set -o pipefail` (e.g. a base tree that predates any tests).
    tests=$({ grep -rE '#\[(tokio::)?test\]' "$target/crates" 2>/dev/null || true; } | wc -l | tr -d ' ')

    echo ""
    echo "**transitive deps:** $ext_deps &nbsp;·&nbsp; **workspace crates:** $members &nbsp;·&nbsp; **tests:** $tests"
    echo ""
    echo "<!--TOTALS loc=$total_loc ext_deps=$ext_deps crates=$members tests=$tests-->"
}

# field <key> : read a TOTALS value from stdin.
field() { grep -o 'TOTALS[^-]*' | tr ' ' '\n' | grep "^$1=" | cut -d= -f2; }

case "${1:-}" in
    --base)
        ref="${2:?usage: metrics.sh --base <git-ref>}"
        head_out=$(emit "$repo_root")
        # Worktree into a fresh subpath: `git worktree add` refuses a path that
        # already exists, so we cannot hand it the `mktemp -d` dir itself. The
        # EXIT trap removes the worktree and temp dir even if a later command
        # aborts under `set -e` (bad ref, or emit failing on the base tree).
        tmp=$(mktemp -d)
        wt="$tmp/base"
        trap 'git -C "$repo_root" worktree remove --force "$wt" 2>/dev/null || true; rm -rf "$tmp"' EXIT
        git -C "$repo_root" worktree add -q --detach "$wt" "$ref"
        base_out=$(emit "$wt")

        # `|| true` keeps a missing key from aborting the capture under set -e;
        # `${x:-0}` keeps an empty value from becoming a literal empty operand in
        # the arithmetic (`$(( 56 -  ))` is a fatal syntax error).
        head_loc=$(printf '%s' "$head_out" | field loc || true)
        base_loc=$(printf '%s' "$base_out" | field loc || true)
        head_dep=$(printf '%s' "$head_out" | field ext_deps || true)
        base_dep=$(printf '%s' "$base_out" | field ext_deps || true)
        dloc=$(( ${head_loc:-0} - ${base_loc:-0} ))
        ddep=$(( ${head_dep:-0} - ${base_dep:-0} ))
        sign() { if [ "$1" -gt 0 ]; then echo "+$1"; else echo "$1"; fi; }
        echo "**Δ vs \`$ref\`: $(sign "$dloc") rust loc · $(sign "$ddep") transitive deps**"
        echo ""
        printf '%s\n' "$head_out"
        ;;
    "")  emit "$repo_root" ;;
    *)   emit "$1" ;;
esac
