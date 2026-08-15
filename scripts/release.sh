#!/usr/bin/env bash
# Release script with a hard CI gate (internal/dev tool, not shipped to users).
#
# Prevents the 2026-08 incident where v0.1 was released while CI was red:
# a release is only created when the target commit's CI is fully green.
#
# Usage:
#   scripts/release.sh            # release the current origin/main HEAD
#   scripts/release.sh <commit>   # release a specific commit (must be on main)
#
# Steps:
#   1. local state checks  (on main, clean tree, versions consistent)
#   2. build + tests       (fmt / clippy / test — the same gates CI runs)
#   3. CI gate             (every CI run for the commit is completed + success)
#   4. tag vX.Y.Z + push
#   5. create GitHub release with notes from `git log` since the last tag
set -euo pipefail

cd "$(dirname "$0")/.."

GREEN=$'\033[32m'; RED=$'\033[31m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
say()  { printf '%s\n' "$*"; }
ok()   { printf '%s[OK]%s %s\n' "$GREEN" "$RESET" "$*"; }
fail() { printf '%s[FAIL]%s %s\n' "$RED" "$RESET" "$*" >&2; }
warn() { printf '%s[WARN]%s %s\n' "$YELLOW" "$RESET" "$*"; }
die()  { fail "$*"; exit 1; }

command -v gh >/dev/null || die "gh CLI is required (https://cli.github.com)"
command -v cargo >/dev/null || die "cargo is required"

TARGET="${1:-$(git rev-parse origin/main)}"
say "${BOLD}Releasing commit ${TARGET}${RESET}"
say

# ---- 1. local state -------------------------------------------------------
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || die "must run on main (HEAD is '$(git rev-parse --abbrev-ref HEAD)')"
git fetch origin >/dev/null 2>&1 || die "git fetch origin failed"
[ "$(git rev-parse origin/main)" = "$(git rev-parse main)" ] || die "local main is not in sync with origin/main (push first)"
[ -z "$(git status --porcelain)" ] || die "working tree is dirty; commit or stash first"

# The target must be an ancestor of origin/main (releasing history off main is not allowed).
git merge-base --is-ancestor "$TARGET" origin/main || die "target $TARGET is not an ancestor of origin/main"

# ---- 2. version consistency + local gates ---------------------------------
cargo_ver=$(grep -m1 '^version' Cargo.toml | sed 's/.*= *"\(.*\)"/\1/')
plugin_ver=$(grep -m1 '^version' herdr-plugin.toml | sed 's/.*= *"\(.*\)"/\1/')
lock_ver=$(grep -m1 -A1 'name = "herdr-auto-update"' Cargo.lock | grep '^version' | sed 's/.*= *"\(.*\)"/\1/')
[ "$cargo_ver" = "$plugin_ver" ] || die "version mismatch: Cargo.toml=$cargo_ver herdr-plugin.toml=$plugin_ver"
[ "$cargo_ver" = "$lock_ver" ] || die "version mismatch: Cargo.toml=$cargo_ver Cargo.lock=$lock_ver"
ok "versions consistent ($cargo_ver)"

cargo fmt --check >/dev/null 2>&1 || die "cargo fmt --check failed"
cargo clippy --all-targets -- -D warnings >/dev/null 2>&1 || die "clippy failed"
cargo test --all-targets >/dev/null 2>&1 || die "tests failed"
ok "fmt / clippy / tests green locally"

# ---- 3. CI gate ------------------------------------------------------------
say "checking CI status for $TARGET ..."
runs=$(gh run list --commit "$TARGET" --json databaseId,status,conclusion,displayTitle 2>/dev/null || true)
if [ -z "$runs" ] || [ "$(printf '%s' "$runs" | grep -c databaseId)" -eq 0 ]; then
    die "no CI runs found for $TARGET — CI has not run, refusing to release"
fi
while read -r run; do
    id=$(printf '%s' "$run" | jq -r .databaseId)
    status=$(printf '%s' "$run" | jq -r .status)
    conclusion=$(printf '%s' "$run" | jq -r .conclusion)
    title=$(printf '%s' "$run" | jq -r .displayTitle)
    if [ "$status" != "completed" ]; then
        die "run $id ($title) is $status, not completed — wait for CI before releasing"
    fi
    if [ "$conclusion" != "success" ]; then
        die "run $id ($title) concluded $conclusion — CI is RED, refusing to release (see https://github.com/dio16/herdr-auto-update/actions/runs/$id)"
    fi
    ok "CI run $id ($title) green"
done < <(printf '%s' "$runs" | jq -c '.[]')

# ---- 4. tag + release --------------------------------------------------------
prev_tag=$(git describe --tags --abbrev=0 "$TARGET"^ 2>/dev/null || true)
tag="v$cargo_ver"
if git rev-parse "$tag" >/dev/null 2>&1; then
    die "tag $tag already exists"
fi
notes="## Changes since $prev_tag

$(git log --oneline "${prev_tag:-$(git rev-list --max-parents=0 "$TARGET")}..$TARGET")"

git tag "$tag" "$TARGET"
git push origin "$tag"
say "created and pushed tag $tag"

printf '%s' "$notes" | gh release create "$tag" --title "$tag" --notes-file -
ok "release $tag created: https://github.com/dio16/herdr-auto-update/releases/tag/$tag"
