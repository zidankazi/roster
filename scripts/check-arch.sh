#!/usr/bin/env bash
#
# check-arch.sh — enforce roster's one-way crate dependency graph.
#
# The crate split is the architecture (see docs/01-crates.md). Dependencies must
# only flow one way; an agent adding an upward or lateral edge would compile fine
# but erode the design. This script fails if any crate's internal (roster-*)
# dependencies fall outside the allowlist below.
#
# Adding an edge is a DELIBERATE act: update `allowed_for` here and explain why
# in the PR. That friction is the point — the graph stays known.
#
# Portable to bash 3.2 (macOS default). Only checks [dependencies]; dev- and
# build-dependencies are not part of the shipped architecture.

set -euo pipefail
cd "$(dirname "$0")/.."

# Allowed internal dependencies per crate, space-separated. Empty = leaf crate.
allowed_for() {
    case "$1" in
        roster-core)   echo "" ;;
        roster-proto)  echo "" ;;
        roster-pty)    echo "" ;;
        roster-term)   echo "roster-core" ;;
        roster-detect) echo "roster-core" ;;
        roster-tui)    echo "roster-core roster-detect" ;;
        roster)        echo "roster-core roster-detect roster-proto roster-pty roster-term roster-tui" ;;
        *)             echo "__UNREGISTERED__" ;;
    esac
}

fail=0
for toml in crates/*/Cargo.toml; do
    dir=$(dirname "$toml")
    name=$(basename "$dir")

    allowed=$(allowed_for "$name")
    if [ "$allowed" = "__UNREGISTERED__" ]; then
        echo "UNREGISTERED CRATE: $name is not listed in scripts/check-arch.sh."
        echo "  Add it to allowed_for() with its permitted internal deps."
        fail=1
        allowed=""
    fi

    # Extract roster-* dependency keys from the [dependencies] table only.
    actual=$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' "$toml" \
        | grep -oE '^roster-[a-z]+' | sort -u || true)

    for dep in $actual; do
        if ! printf '%s\n' $allowed | grep -qx "$dep"; then
            echo "ARCH VIOLATION: $name depends on $dep, which is not allowed."
            echo "  allowed for $name: ${allowed:-(none — leaf crate)}"
            echo "  If intentional, update allowed_for() in scripts/check-arch.sh and justify it in the PR."
            fail=1
        fi
    done
done

if [ "$fail" -ne 0 ]; then
    echo
    echo "Crate dependency-graph check FAILED. See docs/01-crates.md for the intended one-way DAG."
    exit 1
fi

echo "Crate dependency graph OK — all internal edges within the allowlist."
