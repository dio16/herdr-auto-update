# herdr-auto-update

A [herdr](https://herdr.dev) plugin that checks your installed plugins for newer
upstream commits and reinstalls them. It runs automatically at herdr server
startup, and can also be triggered manually as a plugin action or a standalone
CLI.

herdr plugin v1 has no dedicated `plugin update` command, so this plugin
performs the update as a reinstall (`herdr plugin install <owner>/<repo>`),
which replaces the managed checkout while preserving plugin config and state.

## Features

- Checks every GitHub-installed plugin against its upstream `HEAD` (via
  `git ls-remote`) and reinstalls the outdated ones. Plugins installed with
  `herdr plugin install --ref` keep their pin: they are compared and
  reinstalled against that same ref, never moved to `HEAD`.
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
- Update policy: `auto` (reinstall), `notify` (report only, never install),
  `pinned-only` (update only pinned plugins), plus `allow` owner/repo globs.
- Dry-run `plan` command and explicit `apply`, both scriptable via `--json`.
- Linux, macOS, and Windows support; Rust binary with argv-based subprocess
  calls (no shell interpolation).

## Requirements

- herdr >= 0.7.0
- `git` on PATH (used for the upstream check; herdr's own plugin install also
  requires it)
- `curl` on PATH (used to classify update direction via the GitHub compare
  API; when unavailable every changed plugin is reported as `unknown`)
- A Rust toolchain on the machine that installs the plugin (the plugin builds
  itself with `cargo build --release` during `herdr plugin install`)

## Install

```bash
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
herdr-auto-update`):

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

# Update policy (default "auto"): what update/apply/startup may do with a
# plugin whose upstream ref changed.
#   "auto"        - reinstall outdated plugins (current behavior)
#   "notify"      - check, report, and notify only; never install
#   "pinned-only" - reinstall only plugins installed with a pinned --ref
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
```

## CLI

When herdr runs the plugin it injects `HERDR_BIN_PATH`; for standalone use the
`herdr` binary is resolved from PATH.

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
(`same`/`behind`/`ahead`/`diverged`/`unknown`), `requested_ref`, `error`).

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

`herdr-auto-update rollback` — reinstall each plugin at its previous commit
(`herdr plugin install <owner>/<repo> --ref <previous-sha>`), using the most
recent recorded state. `rollback --only <plugin_id>` restricts the target.
Plugins without a recorded previous commit are skipped with a warning.

```bash
herdr-auto-update rollback
herdr-auto-update rollback --only herdr-file-viewer
```

### Common flags

| Flag | Description |
|---|---|
| `--json` | machine-readable output (check/update; suppresses notifications) |
| `--config <path>` | override the plugin config file |
| `--only <plugin_id>` | restrict check/update to one plugin id |
| `-V, --version` | print the version |
| `-h, --help` | print help |

### Exit codes

| Code | Meaning |
|---|---|
| `0` | ok / everything up to date |
| `1` | updates available (check) or one or more reinstalls failed (update/startup) |
| `2` | fatal error, bad usage, or one or more plugin checks errored |

## Development

```bash
herdr plugin link /path/to/herdr-auto-update   # local dev registration
cargo build --release
cargo clippy -- -D warnings
cargo test
```

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
- Dependencies are minimal (`serde`, `serde_json`, `toml`), pinned by
  `Cargo.lock` for the binary.

## License

MIT
