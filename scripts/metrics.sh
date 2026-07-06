#!/usr/bin/env bash
#
# metrics.sh — report codebase size and dependency footprint.
#
# Prints a markdown table (per-crate Rust LOC + totals, transitive dependency
# count, workspace crate count, test count) to stdout, plus a machine-readable
# HTML-comment TOTALS line (invisible when rendered). CI runs this on every PR
# so growth is a number the reviewer approves — see .github/workflows/ci.yml.
#
# Optional arg: a target repo root to analyse (defaults to this repo). CI passes
# a checkout of the base branch here to compute the diff, so this script must be
# able to analyse a tree that doesn't itself contain metrics.sh.
#
# Portable to bash 3.2 (macOS default). Needs `cargo` and `jq`.

set -euo pipefail
target="${1:-$(cd "$(dirname "$0")/.." && pwd)}"

echo "| crate | rust loc |"
echo "|---|---:|"
total_loc=0
for dir in "$target"/crates/*/; do
    [ -d "$dir" ] || continue
    name=$(basename "$dir")
    loc=$(find "$dir" -name '*.rs' -type f -print0 2>/dev/null | xargs -0 cat 2>/dev/null | wc -l | tr -d ' ')
    loc=${loc:-0}
    total_loc=$((total_loc + loc))
    printf '| %s | %s |\n' "$name" "$loc"
done
printf '| **total** | **%s** |\n' "$total_loc"

pkgs=$(cargo metadata --format-version 1 --manifest-path "$target/Cargo.toml" 2>/dev/null | jq '.packages | length')
members=$(cargo metadata --format-version 1 --manifest-path "$target/Cargo.toml" 2>/dev/null | jq '.workspace_members | length')
ext_deps=$(( pkgs - members ))
tests=$(grep -rE '#\[(tokio::)?test\]' "$target/crates" 2>/dev/null | wc -l | tr -d ' ')

echo ""
echo "**transitive deps:** $ext_deps &nbsp;·&nbsp; **workspace crates:** $members &nbsp;·&nbsp; **tests:** $tests"
echo ""
echo "<!--TOTALS loc=$total_loc ext_deps=$ext_deps crates=$members tests=$tests-->"
