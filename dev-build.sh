#!/bin/bash
#
# dev-build.sh - run cargo against local sibling checkouts, reliably.
#
# Why this exists: this repo depends on sibling bridge crates as *git*
# dependencies, with gitignored [patch] overrides in .cargo/config.toml
# redirecting them to local checkouts during development. Cargo never lets
# a [patch] override an existing Cargo.lock pin, so a bare `cargo build`
# silently compiles the GitHub revisions of those crates — NOT your local
# edits. Conversely, once the patches do take effect they rewrite
# Cargo.lock with local-path entries that must never be committed (CI has
# no sibling checkouts).
#
# This script keeps two lockfiles and swaps them around the cargo call:
#   Cargo.lock       committed lock, pinned to git sources (CI truth)
#   .cargo/dev.lock  local-only lock, resolved with the patches applied
# Afterwards it verifies every patched crate in the dependency graph
# actually resolved to a local path, and fails loudly if not.
#
# Usage:
#   ./dev-build.sh                  # cargo build
#   ./dev-build.sh test             # cargo test
#   ./dev-build.sh build --release  # any cargo subcommand + args
#
set -euo pipefail
cd "$(dirname "$0")"

CONFIG=.cargo/config.toml
DEV_LOCK=.cargo/dev.lock
CI_LOCK_STASH=.cargo/ci.lock.swap

[[ $# -eq 0 ]] && set -- build

# No local patch overrides: behave exactly like cargo.
if [[ ! -f $CONFIG ]] || ! grep -q '^\[patch\.' "$CONFIG"; then
    exec cargo "$@"
fi

# Crate names the config patches to local paths.
patched=$(sed -n 's/^\([A-Za-z0-9_-]*\) *= *{ *path *=.*/\1/p' "$CONFIG")

swapped=""
restore() {
    if [[ -n $swapped ]]; then
        [[ -f Cargo.lock ]] && mv Cargo.lock "$DEV_LOCK"
        [[ -f $CI_LOCK_STASH ]] && mv "$CI_LOCK_STASH" Cargo.lock
    fi
}
trap restore EXIT

# If the committed (CI) lock is tracked, set it aside and use the dev lock;
# cargo re-creates the dev lock from scratch if it doesn't exist yet, and a
# fresh resolve does honor the config patches.
if git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    swapped=1
    mv Cargo.lock "$CI_LOCK_STASH"
    [[ -f $DEV_LOCK ]] && mv "$DEV_LOCK" Cargo.lock
fi

# True when every patched crate that appears in the lock is path-resolved
# (path-resolved entries are the only ones without a `source =` line).
verify() {
    local ok=0 crate
    for crate in $patched; do
        grep -q "^name = \"$crate\"\$" Cargo.lock 2>/dev/null || continue
        if grep -A2 "^name = \"$crate\"\$" Cargo.lock | grep -q '^source ='; then
            echo "dev-build: $crate still resolves to a remote source" >&2
            ok=1
        fi
    done
    return $ok
}

cargo "$@"

if [[ -f Cargo.lock ]] && ! verify; then
    # Stale dev lock from before the patches existed; it is disposable —
    # discard it and re-resolve fresh, which applies the patches.
    echo "dev-build: discarding stale dev lock and re-resolving..." >&2
    rm Cargo.lock
    cargo "$@"
    verify || {
        echo "dev-build: ERROR: patched crates still resolve to remote sources." >&2
        echo "dev-build: check that the sibling checkouts in $CONFIG exist." >&2
        exit 1
    }
fi

for crate in $patched; do
    if grep -q "^name = \"$crate\"\$" Cargo.lock 2>/dev/null; then
        echo "dev-build: ✓ $crate → local checkout"
    fi
done
