# herdr-auto-update

A [herdr](https://herdr.dev) plugin that checks your installed plugins for newer
upstream commits and reinstalls them - if you opt in. Since v1.0 the default
`policy` is `notify`: updates are detected and reported, but never installed
automatically. Set `policy = "auto"` (or `"pinned-only"`) in the plugin
config to enable automatic reinstalls. Runs as a herdr plugin action and as a
standalone CLI.

herdr plugin v1 has no dedicated `plugin update` command, so this plugin
performs the update as a reinstall (`herdr plugin install <owner>/<repo>`),
which replaces the managed checkout while preserving plugin config and state.

## Features

- Checks every GitHub-installed plugin against its upstream ref (via
  `git ls-remote`) and reinstalls the outdated ones. Plugins installed with
  `herdr plugin install --ref` keep their pin: they are compared and
  reinstalled against that same ref, never moved to `HEAD`.
- Ref channels are explicit: branch / tag / commit pins are classified
  (`channel:` in `plan` output, `ref_kind` in JSON). Commit-pinned plugins
  are immutable by construction - the pin is the installed commit, so they
  are never reinstalled and never hit the network.
- Remote checks run in parallel under a bounded worker pool
  (`max_concurrency`, default 8), so the check time is one network round trip
  regardless of how many plugins are installed.
- Every remote check has a hard wall-clock deadline (`timeout_secs`, default
  20): a wedged git process is killed, so a dead network cannot hang herdr
  startup.
- Automatic check + reinstall at herdr server startup (toggleable).
- Desktop notification when updates ran (`herdr notification show`,
  toggleable).
- Manual actions: `check` / `update` (bind keys in your herdr config).
- Standalone CLI with human and `--json` output and scriptable exit codes,
  including `--only <plugin_id>` to target a single plugin.
- `exclude` list for plugins you do not want touched.
- Update policy: `auto` (reinstall), `notify` (report only, never install;
  the default since v1.0), `pinned-only` (update only pinned plugins), plus
  `allow` owner/repo globs. A one-line stderr note is printed while `policy`
  is unset.
- Dry-run `plan` command and explicit `apply`, both scriptable via `--json`.
- Live progress panel on terminals: an animated per-plugin view (spinner,
  ✓/✗, and an overall progress bar) while checks and updates run. Auto-
  disabled for piped output and `--json`, so herdr logs and scripts see the
  same stable line-based report as before.
- Human-readable version transitions in reports (`v1.0.1 -> v1.0.2`): the
  installed version comes from the plugin manifest, the upstream version
  from the newest release tag. Plugins without versions fall back to commit
  SHAs.
- Linux, macOS, and Windows support; Rust binary with argv-based subprocess
  calls (no shell interpolation).

## Requirements

- herdr >= 0.7.0
- `git` on PATH (used for the upstream check; herdr's own plugin install also
  requires it)
- `curl` on PATH (used to classify update direction via the GitHub compare
  API; when unavailable every changed plugin is reported as `unknown`)
- `gh` CLI or `curl` for the launcher's first-run binary download (Linux and
  macOS also need `tar`; Windows 10+ ships `tar` and `curl`)

## Install

```bash
herdr plugin install dio16/herdr-auto-update
```

No Rust toolchain is required: the manifest has no build step (clone-only
install), and the `bin/` launcher downloads the versioned prebuilt binary for
your platform from GitHub Releases on first use, verifying its SHA256
checksum before running it. The binary is cached in `bin/.cache/`.

If the plugin is already registered from a local dev checkout
(`herdr plugin link`), install from GitHub fails with
`plugin ... is already linked from a local path`. Unlink it first, then
install:

```bash
herdr plugin unlink herdr-auto-update
herdr plugin install dio16/herdr-auto-update
```

Enable the actions you want in `~/.config/herdr/config.toml`
(or `%APPDATA%\herdr\config.toml` on Windows):

```toml
[[keys.command]]
key = "prefix+u"
type = "plugin_action"
command = "herdr-auto-update.update"
description = "Update herdr plugins"
```

On Windows the action ids are suffixed with `-windows`
(`herdr-auto-update.update-windows`, `herdr-auto-update.check-windows`) because
herdr requires action ids to be unique within a plugin. `herdr plugin action
list` prints the exact ids for your platform.

## Configuration

The plugin reads `config.toml` from its plugin config directory (created by
herdr on install; print the path with `herdr plugin config-dir
herdr-auto-update`). When herdr runs the plugin it injects the directory via
env vars; a standalone `herdr-auto-update` run resolves the same directory
through `herdr plugin config-dir`, so shell invocations and herdr's own
startup hook always read the same config:

```toml
# Reinstall outdated plugins automatically at server startup (default true).
auto_update = true

# Show a desktop notification when updates ran (default true). Notifications
# are suppressed when --json is used.
notify = true

# Plugin ids to skip during updates.
exclude = ["flock.farm"]

# Hard wall-clock deadline for each upstream check (git ls-remote), in
# seconds (default 20). Kills a wedged git process so herdr startup cannot
# hang on a dead network connection.
timeout_secs = 20

# Upper bound on concurrent upstream checks (default 8). Registry-wide runs
# never spawn more git processes than this.
max_concurrency = 8

# Update policy (default "notify" since v1.0): what update/apply/startup may
# do with a plugin whose upstream ref changed.
#   "auto"        - reinstall outdated plugins automatically
#   "notify"      - check, report, and notify only; never install
#   "pinned-only" - reinstall only plugins installed with a pinned --ref
# v0.x users: an unset policy now defaults to "notify" (no automatic
# reinstalls) and prints a one-line stderr note. Set `policy = "auto"` below
# to keep the v0.x automatic-update behavior.
policy = "auto"

# Restrict update targets to owner/repo glob patterns (default: all GitHub
# plugins). `*` matches any sequence (including `/`), `?` one non-`/` char.
# allow = ["ragamo/*", "dio16/herdr-*"]

# Only fast-forward updates are ever applied (default true). Classification
# enforces this: `behind` (installed is an ancestor of upstream) is the only
# status that updates; `ahead` / `diverged` / `unknown` are held.
require_fast_forward = true

# Allow installing a diverged upstream (history rewritten by a force push).
# Default false: diverged plugins are held until you decide.
allow_force_push = false

# Only auto-update plugins whose GitHub owner is in this list (default:
# empty = any owner). Use to whitelist trusted plugin authors.
# trusted_owners = ["dio16", "ragamo"]

# Hold tag- and commit-pinned plugins (default true): their pin is treated as
# immutable, so only branch-tracked plugins are ever auto-updated.
# immutable_pins = true
```

## CLI

When herdr runs the plugin it injects `HERDR_BIN_PATH` and runs the command
from the plugin root, so the launcher always finds the binary. For standalone
use the `herdr-auto-update` executable must be on PATH: either
`cargo install --path .` from a checkout (installs into `~/.cargo/bin`), or
add the plugin's `bin/` directory (which contains the downloaded binary under
`.cache/`) to PATH.

### Check for updates

`herdr-auto-update check` — report which GitHub-installed plugins have an
upstream ref (HEAD or the pinned ref) that differs from the installed commit.
Exits `1` when updates are available, `2` when any plugin check errored, `0`
otherwise.

```bash
herdr-auto-update check
```

### Check with JSON output

`herdr-auto-update check --json` — same check, machine-readable output as a
JSON array of plugin statuses (one object per plugin with `plugin_id`, `owner`,
`repo`, `version`, `installed_sha`, `remote_sha`, `update_available`, `status`
(`same`/`behind`/`ahead`/`diverged`/`unknown`), `ref_kind`
(`branch`/`tag`/`commit`), `requested_ref`, `error`).

`status` is classified via the GitHub compare API (`installed...upstream`):
`behind` = upstream has commits you lack (updateable), `ahead` = you have
commits upstream lacks, `diverged` = both sides have unique commits (force
push), `unknown` = classification failed (network/rate limit) — degraded to
the safe side (never auto-updated).

```bash
herdr-auto-update check --json
```

### Update plugins

`herdr-auto-update update` — reinstall every outdated plugin through
`herdr plugin install`. Exits `1` when one or more reinstalls failed, `2` when
any plugin check errored (successfully checked plugins are still updated in
that case). Shows a notification when updates ran (unless `notify = false`).

```bash
herdr-auto-update update
```

### Plan (dry run)

`herdr-auto-update plan` — analyze only, execute nothing: prints the status,
policy, and action (`UPDATE`/`HOLD`/`ERROR`) for every plugin. `plan --json`
emits the same as machine-readable JSON. Exits `1` when updates would apply,
`2` when any plugin check errored, `0` otherwise.

```bash
herdr-auto-update plan
herdr-auto-update plan --json
```

### Apply

`herdr-auto-update apply` — execute the plan's `UPDATE` actions only (identity
with `update`; both exist so scripts can name the phase they mean).

```bash
herdr-auto-update apply
```

### Update one plugin

`herdr-auto-update update --only <plugin_id>` — restrict check/update to a
single plugin id. An id that is not in the registry, or not a GitHub-installed
plugin, is a fatal error (exit `2`).

```bash
herdr-auto-update update --only flock.farm
```

### Startup hook

`herdr-auto-update startup` — what herdr runs at server startup: check and
reinstall outdated plugins per the configured `policy`, unless
`auto_update = false` in the config. Not meant to be run by hand.

```bash
herdr-auto-update startup
```

### Update history

Every applied update is recorded in `state.json` inside the plugin config
directory (append-only; the latest entry per plugin is the rollback target).
`herdr-auto-update history` prints the audit trail; `history --json` emits it
machine-readable.

```bash
herdr-auto-update history
herdr-auto-update history --json
```

### Roll back an update

`herdr-auto-update rollback` — reinstall each plugin at the commit recorded
before its **most recent** update (`herdr plugin install <owner>/<repo>
--ref <previous-sha>`), using state. Older history is never rewound: an
A→B→C trail rolls back to B, not to A. `rollback --only <plugin_id>`
restricts the target. Plugins without a recorded previous commit are
skipped with a warning; a plugin that is already rolled back is left alone.

Rolling back pin installs the previous commit, so herdr records it as a
commit pin (`--ref <sha>`). By design that **quarantines** the plugin: it is
no longer auto-updated, and `plan` marks it "pinned by rollback". To rejoin
the tracking ref (branch or default) that was recorded before the rollback,
run `resume` — it reinstalls from that ref and the plugin becomes an
auto-update candidate again.

```bash
herdr-auto-update rollback
herdr-auto-update rollback --only herdr-file-viewer
herdr-auto-update resume
herdr-auto-update resume --only herdr-file-viewer
```

Every rollback and resume is appended to `state.json`, so `history` shows
the full trail: `updated` → `rolled_back` → `updated`.

### Pinned plugins (commit pins)

A plugin whose `requested_ref` is a commit SHA is **pinned to a commit**:
it is never auto-updated (the pin IS the installed commit). `check`/`plan`
show `channel: commit`, and `update`/`startup` report it as `pinned` (JSON
report field, stderr summary, and desktop notification) instead of silently
listing it as up to date.

This happens after a `rollback` (rejoin with `resume`) and for installs that
were pinned to a specific commit. To switch a commit-pinned plugin back to
default-branch tracking, reinstall it without a ref:

```bash
herdr-auto-update untrack --only <plugin_id>
```

`untrack` reinstalls the plugin through herdr without `--ref` and verifies
the pin is gone. It requires `--only` (one plugin at a time) and exits `2`
if the plugin is not commit-pinned or not in the registry.

### Common flags

| Flag | Description |
|---|---|
| `--json` | machine-readable output (check/update; suppresses notifications) |
| `--config <path>` | override the plugin config file |
| `--only <plugin_id>` | restrict check/update/rollback/resume to one plugin id; required by `untrack` |
| `-V, --version` | print the version |
| `-h, --help` | print help |

### Exit codes

| Code | Meaning |
|---|---|
| `0` | ok / everything up to date |
| `1` | updates available (check), updates would apply (plan), one or more reinstalls failed (update/startup), or nothing to roll back / resume |
| `2` | fatal error, bad usage, or one or more plugin checks errored; `untrack` misuse or a plugin that is not commit-pinned |

## Development

```bash
herdr plugin link /path/to/herdr-auto-update   # local dev registration
cargo build --release
cargo clippy -- -D warnings
cargo test
```

With a local `target/release/herdr-auto-update` present, the `bin/` launcher
runs that build directly (no download); without it, the launcher downloads the
versioned prebuilt binary for your platform from GitHub Releases, verifying
its SHA256 checksum (release assets: `herdr-auto-update-<version>-<triple>.tar.gz`
+ `checksums-<version>.txt`, built by `.github/workflows/release.yml` on tag
pushes). The dev launchers `scripts/run.sh` / `scripts/run.ps1` remain for
cargo-built installs.

On first run the launcher installs a `~/.local/bin/herdr-auto-update` symlink
(set `XDG_BIN_HOME` to override) so the CLI works as a standalone command
after `herdr plugin install`; on Windows it writes
`%LOCALAPPDATA%\Microsoft\WindowsApps\herdr-auto-update.cmd`. The plugin root
is a stable hash of the plugin id, so the shim survives reinstalls and is
re-created if it goes stale. Remove the shim when uninstalling the plugin.

To be listed in the herdr marketplace, add the `herdr-plugin` topic to the
repository on GitHub (the index refreshes automatically).

## Security notes

- All subprocess execution uses argv arrays; plugin ids, owners, and repos
  from the registry are validated (`[A-Za-z0-9._-]`, length-bounded) before
  being placed into URLs or arguments.
- The plugin never evaluates shell code; subprocesses are `git`, `curl`, and
  herdr's own installer. The only files it writes are `state.json` and
  `compare-cache.json` in herdr's plugin config directory (needed for
  rollback and rate-limit-safe classification).
- Prebuilt binaries are downloaded from the `dio16/herdr-auto-update`
  GitHub release matching the pinned version and verified against the
  release's SHA256 checksum before execution. The binary is cached under
  `bin/.cache/` (gitignored). The `bin/` launcher falls back to a local
  `cargo build --release` when present, so `herdr plugin link` dev setups
  never download anything.
- Dependencies are minimal (`serde`, `serde_json`, `toml`), pinned by
  `Cargo.lock` for the binary.

## License

MIT
