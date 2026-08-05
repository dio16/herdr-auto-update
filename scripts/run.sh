#!/bin/sh
# herdr-auto-update launcher (POSIX: Linux/macOS). Self-locating so it works
# regardless of herdr's working directory. Runs the cargo-built binary.
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$DIR/../target/release/herdr-auto-update" "$@"
