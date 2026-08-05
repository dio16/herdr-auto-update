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
- Remote checks run in parallel, so the check time is one network round trip
  regardless of how many plugins are installed.
- Automatic check + reinstall at herdr server startup (toggleable).
- Desktop notification when updates ran (`herdr notification show`,
  toggleable).
- Manual actions: `check` / `update` (bind keys in your herdr config).
- Standalone CLI with human and `--json` output and scriptable exit codes,
  including `--only <plugin_id>` to target a single plugin.
- `exclude` list for plugins you do not want touched.
- Linux, macOS, and Windows support; Rust binary with argv-based subprocess
  calls (no shell interpolation).

## Requirements

- herdr >= 0.7.0
- `git` on PATH (used for the upstream check; herdr's own plugin install also
  requires it)
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
```

## CLI

When herdr runs the plugin it injects `HERDR_BIN_PATH`; for standalone use the
`herdr` binary is resolved from PATH.

### Check for updates

`herdr-auto-update check` — report which GitHub-installed plugins have newer
commits upstream. Exits `1` when updates are available, `2` when any plugin
check errored, `0` otherwise.

```bash
herdr-auto-update check
```

### Check with JSON output

`herdr-auto-update check --json` — same check, machine-readable output as a
JSON array of plugin statuses (one object per plugin with `plugin_id`, `owner`,
`repo`, `version`, `installed_sha`, `remote_sha`, `update_available`,
`requested_ref`, `error`).

```bash
herdr-auto-update check --json
```

### Update plugins

`herdr-auto-update update` — reinstall every outdated plugin through
`herdr plugin install`. Exits `1` when one or more reinstalls failed. Shows a
notification when updates ran (unless `notify = false`).

```bash
herdr-auto-update update
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
reinstall outdated plugins, unless `auto_update = false` in the config. Not
meant to be run by hand.

```bash
herdr-auto-update startup
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
- The plugin never writes files and never evaluates shell code; the only
  writes that happen are performed by herdr's own installer.
- Dependencies are minimal (`serde`, `serde_json`, `toml`), pinned by
  `Cargo.lock` for the binary.

## License

MIT
