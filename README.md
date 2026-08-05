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
  `git ls-remote`) and reinstalls the outdated ones.
- Automatic check + reinstall at herdr server startup (toggleable).
- Manual actions: `check` / `update` (bind keys in your herdr config).
- Standalone CLI with human and `--json` output and scriptable exit codes.
- `exclude` list for plugins you do not want touched.
- Windows and Linux (WSL) support; Rust binary with argv-based subprocess
  calls (no shell interpolation).

## Requirements

- herdr >= 0.7.0
- `git` on PATH (used for the upstream check; herdr's own plugin install also
  requires it)
- A Rust toolchain on the machine that installs the plugin (the plugin builds
  itself with `cargo build --release` during `herdr plugin install`)

## Install

```bash
herdr plugin install <owner>/herdr-auto-update
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

## Configuration

The plugin reads `config.toml` from its plugin config directory (created by
herdr on install; print the path with `herdr plugin config-dir
herdr-auto-update`):

```toml
# Reinstall outdated plugins automatically at server startup (default true).
auto_update = true

# Plugin ids to skip during updates.
exclude = ["flock.farm"]
```

## CLI

```text
herdr-auto-update <startup|check|update> [--json] [--config <path>]

exit codes:
  0  ok / everything up to date
  1  updates available (check) or one or more reinstalls failed (update/startup)
  2  fatal error (herdr CLI unavailable, unparsable registry, bad usage)
```

When herdr runs the plugin it injects `HERDR_BIN_PATH`; for standalone use the
`herdr` binary is resolved from PATH.

## Development

```bash
herdr plugin link /path/to/herdr-auto-update   # local dev registration
cargo build --release
cargo clippy -- -D warnings
cargo test
```

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
