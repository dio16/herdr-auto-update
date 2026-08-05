# herdr-auto-update launcher (Windows). Self-locating via $PSScriptRoot so it
# works regardless of herdr's working directory. Runs the cargo-built binary.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\herdr-auto-update.exe"
& $exe @args
exit $LASTEXITCODE
