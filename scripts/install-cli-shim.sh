#!/bin/sh
# Post-install hook: put the standalone CLI on the user's PATH so
# `herdr-auto-update check` works from a shell right after
# `herdr plugin install` (v1.0.6). herdr runs [[build]] commands at install
# time; this one compiles nothing - it creates a symlink from the plugin
# root's bin/ launcher into the user's bin dir. Best-effort and silent:
# exits 0 even when the shim cannot be created, so install never fails over
# a convenience. The launcher re-heals a dangling or stale shim on later
# runs, and `herdr plugin uninstall` leaves the shim behind for the user to
# remove (documented in the README).

set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PLUGIN_ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)

if [ -n "${XDG_BIN_HOME:-}" ] || [ -n "${HOME:-}" ]; then
    shim_dir=${XDG_BIN_HOME:-"$HOME/.local/bin"}
    mkdir -p "$shim_dir" 2>/dev/null || true
    ln -sfn "$PLUGIN_ROOT/bin/herdr-auto-update" "$shim_dir/herdr-auto-update" 2>/dev/null || true
fi

exit 0
