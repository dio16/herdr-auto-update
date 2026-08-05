// Integration tests: run the real binary against stub `herdr` and `git`
// executables so no network or live herdr is required. Each scenario uses a
// private temp dir; environment overrides are passed per-child via
// Command::env, so tests are safe to run in parallel.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REGISTRY: &str = r#"{"id":"cli:plugin","result":{"plugins":[
  {"plugin_id":"flock.farm","version":"0.1.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3"}},
  {"plugin_id":"herdr-file-viewer","version":"1.14.0","source":{"kind":"github","owner":"smarzban","repo":"herdr-file-viewer","resolved_commit":"350f3f5be79d136933ba36c8c8dd60f79df28002"}},
  {"plugin_id":"wave-tui.radio","version":"0.1.5","source":{"kind":"github","owner":"takemo101","repo":"wave-tui","resolved_commit":"f282ba19165f812baa081a5a1dfc0b2403348185"}},
  {"plugin_id":"linked-dev","version":"0.1.0","source":{"kind":"local","path":"C:\\dev\\some-plugin"}},
  {"plugin_id":"local-thing","version":"1.0.0"},
  {"plugin_id":"broken","source":{"kind":"github","repo":"x","resolved_commit":"0000000000000000000000000000000000000000"}},
  {"plugin_id":"evil","source":{"kind":"github","owner":"bad;rm -rf","repo":"x","resolved_commit":"0000000000000000000000000000000000000000"}}
]}}"#;

// stub git answers: herdr-flock = same sha as installed (up to date),
// herdr-file-viewer = newer sha (update available), anything else = failure.
const STUB_GIT_SH: &str = r#"#!/bin/sh
url=""
for a in "$@"; do
  case "$a" in
    https://github.com/*) url="$a" ;;
  esac
done
case "$url" in
  *herdr-flock*) echo "ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD" ;;
  *herdr-file-viewer*) echo "71d4c1c3706e7958c714789b035a99d949620a9e HEAD" ;;
  *) exit 1 ;;
esac
"#;

const STUB_GIT_CMD: &str = "@echo off\r\n\
echo %* | findstr /c:\"herdr-flock\" >nul\r\n\
if not errorlevel 1 ( echo ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD & exit /b 0 )\r\n\
echo %* | findstr /c:\"herdr-file-viewer\" >nul\r\n\
if not errorlevel 1 ( echo 71d4c1c3706e7958c714789b035a99d949620a9e HEAD & exit /b 0 )\r\n\
exit /b 1\r\n";

const STUB_HERDR_SH: &str = r#"#!/bin/sh
if [ "$1" = "plugin" ] && [ "$2" = "list" ]; then
  cat "$(dirname "$0")/registry.json"
  exit 0
fi
if [ "$1" = "plugin" ] && [ "$2" = "install" ]; then
  echo "$3" >> "$(dirname "$0")/installs.log"
  exit 0
fi
exit 1
"#;

const STUB_HERDR_CMD: &str = "@echo off\r\n\
if \"%1\"==\"plugin\" if \"%2\"==\"list\" (\r\n\
  type \"%~dp0registry.json\"\r\n\
  exit /b 0\r\n\
)\r\n\
if \"%1\"==\"plugin\" if \"%2\"==\"install\" (\r\n\
  echo %3>> \"%~dp0installs.log\"\r\n\
  exit /b 0\r\n\
)\r\n\
exit /b 1\r\n";

fn setup(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hau-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("registry.json"), REGISTRY).unwrap();

    if cfg!(windows) {
        std::fs::write(dir.join("stub-git.cmd"), STUB_GIT_CMD).unwrap();
        std::fs::write(dir.join("stub-herdr.cmd"), STUB_HERDR_CMD).unwrap();
    } else {
        std::fs::write(dir.join("stub-git.sh"), STUB_GIT_SH).unwrap();
        std::fs::write(dir.join("stub-herdr.sh"), STUB_HERDR_SH).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("stub-git.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("stub-herdr.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    dir
}

fn run(dir: &Path, args: &[&str], config: Option<&Path>) -> Output {
    let stub_git = dir.join("stub-git.cmd");
    let stub_git = if stub_git.exists() {
        stub_git
    } else {
        dir.join("stub-git.sh")
    };
    let stub_herdr = dir.join("stub-herdr.cmd");
    let stub_herdr = if stub_herdr.exists() {
        stub_herdr
    } else {
        dir.join("stub-herdr.sh")
    };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr-auto-update"));
    cmd.args(args)
        .env("HERDR_BIN_PATH", &stub_herdr)
        .env("HERDR_AUTO_UPDATE_GIT", &stub_git)
        .env_remove("HERDR_PLUGIN_CONFIG_DIR")
        .env_remove("HERDR_PLUGIN_ROOT");
    if let Some(cfg) = config {
        cmd.arg("--config").arg(cfg);
    }
    cmd.output().unwrap()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn installs(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("installs.log")).unwrap_or_default()
}

#[test]
fn check_reports_status_and_exit_1_when_update_pending() {
    let dir = setup("check");
    let out = run(&dir, &["check"], None);
    assert_eq!(out.status.code(), Some(1));
    let s = stdout_of(&out);
    assert!(s.contains("up to date (ae24844b)"), "flock: {s}");
    assert!(s.contains("update available"), "file-viewer: {s}");
    assert!(s.contains("cannot resolve remote HEAD"), "wave-tui: {s}");
    assert!(s.contains("invalid owner/repo"), "evil: {s}");
    assert!(
        s.contains("github source missing owner/repo/commit fields"),
        "broken: {s}"
    );
}

#[test]
fn check_json_is_valid_and_marked() {
    let dir = setup("check-json");
    let out = run(&dir, &["check", "--json"], None);
    assert_eq!(out.status.code(), Some(1));
    let s = stdout_of(&out);
    assert!(s.trim_start().starts_with('['), "json array expected: {s}");
    assert!(s.contains("\"plugin_id\": \"herdr-file-viewer\""), "{s}");
    assert!(s.contains("\"update_available\": true"), "{s}");
    assert!(s.contains("\"plugin_id\": \"evil\""), "{s}");
    assert!(s.contains("\"error\": \"invalid owner/repo"), "{s}");
}

#[test]
fn update_reinstalls_only_outdated_via_herdr_cli() {
    let dir = setup("update");
    let out = run(&dir, &["update"], None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = installs(&dir);
    assert!(
        log.contains("smarzban/herdr-file-viewer"),
        "install log: {log}"
    );
    assert!(
        !log.contains("ragamo/herdr-flock"),
        "up-to-date plugin must not be reinstalled: {log}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 updated, 0 failed"));
}

#[test]
fn update_respects_exclude_list() {
    let dir = setup("update-exclude");
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        "auto_update = true\nexclude = [\"herdr-file-viewer\"]\n",
    )
    .unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        installs(&dir),
        "",
        "excluded plugin must not be reinstalled"
    );
    assert!(stdout_of(&out).contains("excluded"));
}

#[test]
fn startup_honors_auto_update_disabled() {
    let dir = setup("startup-off");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "auto_update = false\n").unwrap();
    let out = run(&dir, &["startup"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(installs(&dir), "", "auto_update=false must not install");
}

#[test]
fn startup_updates_by_default() {
    let dir = setup("startup-on");
    let out = run(&dir, &["startup"], None);
    assert_eq!(out.status.code(), Some(0));
    assert!(installs(&dir).contains("smarzban/herdr-file-viewer"));
}

#[test]
fn update_json_reports_actions() {
    let dir = setup("update-json");
    let out = run(&dir, &["update", "--json"], None);
    assert_eq!(out.status.code(), Some(0));
    let s = stdout_of(&out);
    assert!(
        s.contains("\"updated\": [\n    \"herdr-file-viewer\""),
        "{s}"
    );
    assert!(s.contains("\"failed\": []"), "{s}");
}

#[test]
fn unknown_command_is_usage_error() {
    let dir = setup("usage");
    let out = run(&dir, &["frobnicate"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("USAGE"));
}

#[test]
fn config_flag_without_value_is_usage_error() {
    let dir = setup("config-no-value");
    let out = run(&dir, &["check", "--config"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("USAGE"));
}

#[test]
fn config_flag_with_flag_as_value_is_usage_error() {
    let dir = setup("config-flag-value");
    let out = run(&dir, &["check", "--config", "--json"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("USAGE"));
}

#[test]
fn missing_herdr_cli_is_fatal() {
    let dir = setup("no-herdr");
    let out = Command::new(env!("CARGO_BIN_EXE_herdr-auto-update"))
        .args(["check"])
        .env("HERDR_BIN_PATH", dir.join("does-not-exist.exe"))
        .env_remove("HERDR_PLUGIN_CONFIG_DIR")
        .env_remove("HERDR_PLUGIN_ROOT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot run"));
}
