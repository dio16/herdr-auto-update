# herdr-auto-update launcher shim (Windows).
#
# Same contract as bin/herdr-auto-update (POSIX): resolve the binary from a
# local release build, a cached download, or a fresh download verified against
# the published SHA256 checksum, then run it with all arguments forwarded.

$ErrorActionPreference = "Stop"

$DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
$PLUGIN_ROOT = Split-Path -Parent $DIR

# Standalone CLI shim (v1.0.5): expose the launcher on PATH so
# `herdr-auto-update check` works from a console. %LOCALAPPDATA%\Microsoft\
# WindowsApps is user-writable and on the Windows PATH. Best-effort and
# silent - shim setup must never break a plugin action. The plugin root is a
# stable hash of the plugin id, so the shim survives reinstalls; a stale
# embedded path is rewritten.
if ($env:LOCALAPPDATA) {
    $shimDir = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps"
    $shim = Join-Path $shimDir "herdr-auto-update.cmd"
    $launcher = Join-Path $DIR "herdr-auto-update.ps1"
    $expected = "@echo off`r`npowershell -NoProfile -ExecutionPolicy Bypass -File `"$launcher`" %*`r`n"
    try {
        if (-not (Test-Path $shimDir)) { New-Item -ItemType Directory -Path $shimDir -Force | Out-Null }
        $needsWrite = -not (Test-Path $shim)
        if (-not $needsWrite) {
            try { $needsWrite = (Get-Content -Raw $shim) -ne $expected } catch { $needsWrite = $true }
        }
        if ($needsWrite) { [System.IO.File]::WriteAllText($shim, $expected) }
    } catch {
        # ignore: shim is a convenience, never a dependency
    }
}

# SHA256 hex of a file via pure .NET. Deliberately NOT Get-FileHash: when the
# herdr server is launched from PowerShell 7 (pwsh), its inherited PSModulePath
# lists pwsh 7 module dirs first, so Windows PowerShell 5.1 resolves the pwsh 7
# Microsoft.PowerShell.Utility copy and Get-FileHash becomes unresolvable
# (CommandNotFoundException) while other Utility cmdlets still work. .NET types
# (mscorlib) resolve regardless of PSModulePath, so this works in every
# PowerShell and every herdr launch context.
function Get-FileSha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($Path)
        try {
            $hash = $sha.ComputeHash($stream)
            return ([System.BitConverter]::ToString($hash)).Replace('-', '').ToLowerInvariant()
        } finally {
            $stream.Dispose()
        }
    } finally {
        $sha.Dispose()
    }
}

# 1. Local release build (dev path).
$dev = Join-Path $PLUGIN_ROOT "target\release\herdr-auto-update.exe"
if (Test-Path $dev) {
    & $dev @args
    exit $LASTEXITCODE
}

# Version and target triple are baked into the asset name; keep in sync with
# .github/workflows/release.yml.
$manifest = Join-Path $PLUGIN_ROOT "herdr-plugin.toml"
$VERSION = (Select-String -Path $manifest -Pattern '^version = "([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

$os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$TRIPLE = if ($os -like "*Windows*") {
    "x86_64-pc-windows-msvc"
} elseif ($os -like "*Darwin*") {
    if ($arch -eq "Arm64") { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" }
} else {
    if ($arch -eq "Arm64") { "aarch64-unknown-linux-gnu" } else { "x86_64-unknown-linux-gnu" }
}

$ASSET = "herdr-auto-update-$VERSION-$TRIPLE.tar.gz"
$REPO = "dio16/herdr-auto-update"

$CACHE_DIR = Join-Path $DIR ".cache"
$CACHED_BIN = Join-Path $CACHE_DIR "herdr-auto-update-$VERSION-$TRIPLE.exe"

# 2. Cached download from a previous run.
if (Test-Path $CACHED_BIN) {
    & $CACHED_BIN @args
    exit $LASTEXITCODE
}

# 3. Fresh download + SHA256 verification.
$TMP = Join-Path $env:TEMP ("hau-" + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TMP | Out-Null

try {
    if (Get-Command gh -ErrorAction SilentlyContinue) {
        gh release download "v$VERSION" --repo $REPO --pattern $ASSET --pattern "checksums-$VERSION.txt" --dir $TMP
    } else {
        $base = "https://github.com/$REPO/releases/download/v$VERSION"
        Invoke-WebRequest -Uri "$base/$ASSET" -OutFile (Join-Path $TMP $ASSET)
        Invoke-WebRequest -Uri "$base/checksums-$VERSION.txt" -OutFile (Join-Path $TMP "checksums-$VERSION.txt")
    }

    $tarball = Join-Path $TMP $ASSET
    $expected = (Get-Content (Join-Path $TMP "checksums-$VERSION.txt") | Where-Object { $_ -match "\s$ASSET$" } | Select-Object -First 1) -split '\s+' | Select-Object -First 1
    $actual = Get-FileSha256Hex $tarball
    if (-not $expected -or $expected -ne $actual) {
        Write-Error "herdr-auto-update: checksum mismatch for $ASSET (expected $expected, got $actual)"
    }

    tar -xzf $tarball -C $TMP
    if (-not (Test-Path (Join-Path $TMP "herdr-auto-update.exe"))) {
        Write-Error "herdr-auto-update: $ASSET did not unpack to the expected 'herdr-auto-update.exe' binary."
    }

    New-Item -ItemType Directory -Path $CACHE_DIR -Force | Out-Null
    Move-Item (Join-Path $TMP "herdr-auto-update.exe") $CACHED_BIN
    Write-Host "herdr-auto-update: installed prebuilt binary $VERSION ($TRIPLE); launching."
    & $CACHED_BIN @args
    exit $LASTEXITCODE
} finally {
    Remove-Item -Recurse -Force $TMP -ErrorAction SilentlyContinue
}
