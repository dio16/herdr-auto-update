# Post-install hook (Windows): create the standalone CLI shim so
# `herdr-auto-update check` works from a console right after
# `herdr plugin install` (v1.0.6). herdr runs [[build]] commands at install
# time; this one compiles nothing - it writes a .cmd shim into the
# user-writable %LOCALAPPDATA%\Microsoft\WindowsApps dir (on the Windows
# PATH). Best-effort and silent: never fails the install.

$ErrorActionPreference = "SilentlyContinue"
$DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
$PLUGIN_ROOT = Split-Path -Parent $DIR

if ($env:LOCALAPPDATA) {
    $shimDir = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps"
    $shim = Join-Path $shimDir "herdr-auto-update.cmd"
    $launcher = Join-Path $PLUGIN_ROOT "bin\herdr-auto-update.ps1"
    try {
        if (-not (Test-Path $shimDir)) { New-Item -ItemType Directory -Path $shimDir -Force | Out-Null }
        $content = "@echo off`r`npowershell -NoProfile -ExecutionPolicy Bypass -File `"$launcher`" %*`r`n"
        [System.IO.File]::WriteAllText($shim, $content)
    } catch {}
}

exit 0
