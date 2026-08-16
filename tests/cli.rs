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
  {"plugin_id":"evil","source":{"kind":"github","owner":"bad;rm -rf","repo":"x","resolved_commit":"0000000000000000000000000000000000000000"}},
  {"plugin_id":"pinned.stable","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"1111111111111111111111111111111111111111","requested_ref":"refs/heads/stable"}},
  {"plugin_id":"pinned.old","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"2222222222222222222222222222222222222222","requested_ref":"refs/heads/stable"}}
]}}"#;

// stub git answers: herdr-flock = same sha as installed (up to date),
// herdr-file-viewer = newer sha (update available), anything else = failure.
// A `refs/*` argument (pinned plugin) resolves herdr-flock to a different sha.
const STUB_GIT_SH: &str = r#"#!/bin/sh
url=""
ref=""
for a in "$@"; do
  case "$a" in
    https://github.com/*) url="$a" ;;
    refs/*) ref="$a" ;;
  esac
done
case "$*" in
  *--tags*)
    case "$url" in
      *herdr-flock*) echo "1111111111111111111111111111111111111111 refs/tags/v0.1.0" ;;
      *herdr-file-viewer*) echo "2222222222222222222222222222222222222222 refs/tags/v1.15.0" ;;
      *) exit 1 ;;
    esac
    exit 0
    ;;
esac
case "$url" in
  *herdr-flock*)
    if [ -n "$ref" ]; then
      echo "1111111111111111111111111111111111111111 HEAD"
    else
      echo "ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD"
    fi
    ;;
  *herdr-file-viewer*) echo "71d4c1c3706e7958c714789b035a99d949620a9e HEAD" ;;
  *) exit 1 ;;
esac
"#;

const STUB_GIT_CMD: &str = "@echo off\r\n\
set \"has_ref=no\"\r\n\
for %%a in (%*) do (\r\n\
  echo %%a | findstr /c:\"refs/\" >nul\r\n\
  if not errorlevel 1 set \"has_ref=yes\"\r\n\
)\r\n\
echo %* | findstr /c:\"--tags\" >nul\r\n\
if not errorlevel 1 (\r\n\
  echo %* | findstr /c:\"herdr-flock\" >nul\r\n\
  if not errorlevel 1 ( echo 1111111111111111111111111111111111111111 refs/tags/v0.1.0 & exit /b 0 )\r\n\
  echo %* | findstr /c:\"herdr-file-viewer\" >nul\r\n\
  if not errorlevel 1 ( echo 2222222222222222222222222222222222222222 refs/tags/v1.15.0 & exit /b 0 )\r\n\
  exit /b 1\r\n\
)\r\n\
echo %* | findstr /c:\"herdr-flock\" >nul\r\n\
if not errorlevel 1 (\r\n\
  if \"%has_ref%\"==\"yes\" ( echo 1111111111111111111111111111111111111111 HEAD & exit /b 0 )\r\n\
  echo ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD & exit /b 0\r\n\
)\r\n\
echo %* | findstr /c:\"herdr-file-viewer\" >nul\r\n\
if not errorlevel 1 ( echo 71d4c1c3706e7958c714789b035a99d949620a9e HEAD & exit /b 0 )\r\n\
exit /b 1\r\n";

const STUB_HERDR_SH: &str = r#"#!/bin/sh
if [ "$1" = "plugin" ] && [ "$2" = "list" ]; then
  cat "$(dirname "$0")/registry.json"
  exit 0
fi
if [ "$1" = "plugin" ] && [ "$2" = "install" ]; then
  echo "$@" >> "$(dirname "$0")/installs.log"
  exit 0
fi
if [ "$1" = "plugin" ] && [ "$2" = "config-dir" ]; then
  echo "$(dirname "$0")/plugin-config"
  exit 0
fi
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
  echo "$@" >> "$(dirname "$0")/notifications.log"
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
  echo %*>> \"%~dp0installs.log\"\r\n\
  exit /b 0\r\n\
)\r\n\
if \"%1\"==\"plugin\" if \"%2\"==\"config-dir\" (\r\n\
  echo %~dp0plugin-config\r\n\
  exit /b 0\r\n\
)\r\n\
if \"%1\"==\"notification\" if \"%2\"==\"show\" (\r\n\
  echo %*>> \"%~dp0notifications.log\"\r\n\
  exit /b 0\r\n\
)\r\n\
exit /b 1\r\n";

/// Compare-API stub: every sha mismatch is "ahead" (remote ahead of
/// installed -> fast-forwardable). Individual tests override this file to
/// simulate diverged/ahead verdicts.
const STUB_CURL_SH: &str = r#"#!/bin/sh
echo '{"status":"ahead","ahead_by":1,"behind_by":0,"total_commits":1}'
"#;

const STUB_CURL_CMD: &str = "@echo off\r\n\
echo {\"status\":\"ahead\",\"ahead_by\":1,\"behind_by\":0,\"total_commits\":1}\r\n\
exit /b 0\r\n";

fn setup(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hau-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("registry.json"), REGISTRY).unwrap();

    if cfg!(windows) {
        std::fs::write(dir.join("stub-git.cmd"), STUB_GIT_CMD).unwrap();
        std::fs::write(dir.join("stub-herdr.cmd"), STUB_HERDR_CMD).unwrap();
        std::fs::write(dir.join("stub-curl.cmd"), STUB_CURL_CMD).unwrap();
    } else {
        std::fs::write(dir.join("stub-git.sh"), STUB_GIT_SH).unwrap();
        std::fs::write(dir.join("stub-herdr.sh"), STUB_HERDR_SH).unwrap();
        std::fs::write(dir.join("stub-curl.sh"), STUB_CURL_SH).unwrap();
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
        std::fs::set_permissions(
            dir.join("stub-curl.sh"),
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
    let stub_curl = dir.join("stub-curl.cmd");
    let stub_curl = if stub_curl.exists() {
        stub_curl
    } else {
        dir.join("stub-curl.sh")
    };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr-auto-update"));
    cmd.args(args)
        .env("HERDR_BIN_PATH", &stub_herdr)
        .env("HERDR_AUTO_UPDATE_GIT", &stub_git)
        .env("HERDR_AUTO_UPDATE_CURL", &stub_curl)
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

fn notifications(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("notifications.log")).unwrap_or_default()
}

fn write_registry(dir: &Path, registry: &str) {
    std::fs::write(dir.join("registry.json"), registry).unwrap();
}

/// Write a config with `policy = "auto"` (v1.0's default is notify) plus
/// `extra`, return its path. Most update-path tests assert install behavior,
/// so they opt into the auto policy explicitly.
fn auto_cfg(dir: &Path, extra: &str) -> PathBuf {
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, format!("policy = \"auto\"\n{extra}")).unwrap();
    cfg
}

/// Single github plugin whose stub git hangs forever; used by the
/// hard-timeout test so only one git process is involved.
const SLOW_REGISTRY: &str = r#"{"id":"cli:plugin","result":{"plugins":[
  {"plugin_id":"slow.repo","version":"0.1.0","source":{"kind":"github","owner":"example","repo":"slow-repo","resolved_commit":"0000000000000000000000000000000000000001"}}
]}}"#;

/// Registry without error entries (no unresolvable/evil plugins), used to
/// test exit code 1 for pending updates without errors.
const CLEAN_REGISTRY: &str = r#"{"id":"cli:plugin","result":{"plugins":[
  {"plugin_id":"flock.farm","version":"0.1.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3"}},
  {"plugin_id":"herdr-file-viewer","version":"1.14.0","source":{"kind":"github","owner":"smarzban","repo":"herdr-file-viewer","resolved_commit":"350f3f5be79d136933ba36c8c8dd60f79df28002"}},
  {"plugin_id":"pinned.old","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"2222222222222222222222222222222222222222","requested_ref":"refs/heads/stable"}}
]}}"#;

#[test]
fn check_reports_status_and_exit_2_when_errors() {
    let dir = setup("check");
    let out = run(&dir, &["check"], None);
    // wave-tui (unresolvable), broken (missing fields), evil (invalid name)
    // are errors, which take precedence over pending updates.
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(
        s.contains("up to date (installed v0.1.0, latest v0.1.0)"),
        "flock: {s}"
    );
    assert!(s.contains("update available"), "file-viewer: {s}");
    assert!(s.contains("cannot resolve remote HEAD"), "wave-tui: {s}");
    assert!(s.contains("invalid owner/repo"), "evil: {s}");
    assert!(
        s.contains("github source missing owner/repo/commit fields"),
        "broken: {s}"
    );
}

#[test]
fn check_shows_version_names_not_shas() {
    let dir = setup("check-versions");
    let out = run(&dir, &["check"], None);
    assert_eq!(out.status.code(), Some(2)); // REGISTRY has error entries
    let s = stdout_of(&out);
    // Installed versions come from the registry manifest; the remote version
    // from the newest tag (stub git emits refs/tags/ lines for --tags).
    assert!(
        s.contains("[flock.farm] up to date (installed v0.1.0, latest v0.1.0)"),
        "{s}"
    );
    assert!(
        s.contains("update available: installed v1.14.0 -> latest v1.15.0"),
        "remote version must come from the newest tag: {s}"
    );
    // A plugin without a manifest version keeps the short-SHA fallback.
    assert!(
        s.contains("[pinned.stable] up to date (installed 11111111, latest 11111111)"),
        "{s}"
    );
}

#[test]
fn check_exit_1_when_pending_without_errors() {
    let dir = setup("check-clean");
    write_registry(&dir, CLEAN_REGISTRY);
    let out = run(&dir, &["check"], None);
    assert_eq!(out.status.code(), Some(1));
    let s = stdout_of(&out);
    assert!(s.contains("herdr-file-viewer"), "{s}");
    assert!(!s.contains("error:"), "{s}");
}

#[test]
fn check_json_is_valid_and_marked() {
    let dir = setup("check-json");
    let out = run(&dir, &["check", "--json"], None);
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.trim_start().starts_with('['), "json array expected: {s}");
    assert!(s.contains("\"plugin_id\": \"herdr-file-viewer\""), "{s}");
    assert!(s.contains("\"update_available\": true"), "{s}");
    assert!(s.contains("\"plugin_id\": \"evil\""), "{s}");
    assert!(s.contains("\"error\": \"invalid owner/repo"), "{s}");
}

#[test]
fn check_json_includes_version_and_ref() {
    let dir = setup("check-json-meta");
    let out = run(&dir, &["check", "--json"], None);
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.contains("\"version\": \"1.14.0\""), "{s}");
    assert!(
        s.contains("\"requested_ref\": \"refs/heads/stable\""),
        "{s}"
    );
}

#[test]
fn update_reinstalls_only_outdated_via_herdr_cli() {
    let dir = setup("update");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update"], Some(&cfg));
    // REGISTRY has 3 check errors (wave-tui, broken, evil): update installs
    // what it can and exits 2 so scripts can detect the partial check.
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = installs(&dir);
    assert!(
        log.contains("plugin install smarzban/herdr-file-viewer --yes"),
        "install log: {log}"
    );
    // flock.farm is up to date and pinned.stable matches its pinned ref, so
    // neither may be reinstalled against HEAD; only pinned.old (via --ref)
    // may reinstall herdr-flock.
    assert!(
        !log.contains("plugin install ragamo/herdr-flock --yes"),
        "up-to-date plugin must not be reinstalled against HEAD: {log}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("2 updated, 0 failed, 3 error(s)"));
}

#[test]
fn update_respects_exclude_list() {
    let dir = setup("update-exclude");
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        "policy = \"auto\"\nauto_update = true\nexclude = [\"herdr-file-viewer\"]\n",
    )
    .unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry still has check errors
    let log = installs(&dir);
    assert!(
        !log.contains("herdr-file-viewer"),
        "excluded plugin must not be reinstalled: {log}"
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
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["startup"], Some(&cfg));
    // startup delegates to update: check errors (3 in REGISTRY) -> exit 2.
    assert_eq!(out.status.code(), Some(2));
    assert!(installs(&dir).contains("smarzban/herdr-file-viewer"));
}

#[test]
fn check_shows_installed_and_latest_versions() {
    // Unique setup name: check_shows_version_names_not_shas uses
    // "check-versions"; sharing the temp dir would race the two tests.
    let dir = setup("check-versions2");
    write_registry(&dir, CLEAN_REGISTRY);
    let out = run(&dir, &["check"], None);
    // flock.farm is up to date: both numbers shown, unambiguous.
    let s = stdout_of(&out);
    assert!(
        s.contains("[flock.farm] up to date (installed v0.1.0, latest v0.1.0)"),
        "{s}"
    );
    // herdr-file-viewer is behind: installed -> latest (newest tag), not SHAs.
    assert!(
        s.contains("update available: installed v1.14.0 -> latest v1.15.0"),
        "{s}"
    );
}

#[test]
fn update_json_reports_actions() {
    let dir = setup("update-json");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update", "--json"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry has check errors
    let s = stdout_of(&out);
    assert!(
        s.contains("\"updated\": [\n    \"herdr-file-viewer\""),
        "{s}"
    );
    assert!(s.contains("\"failed\": []"), "{s}");
    // Check errors are part of the report so scripts can count them.
    assert!(s.contains("\"errors\": [\n    \"wave-tui.radio\""), "{s}");
    assert!(s.contains("\"broken\""), "{s}");
    assert!(s.contains("\"evil\""), "{s}");
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
fn check_pinned_compares_against_ref_not_head() {
    let dir = setup("check-pinned");
    let out = run(&dir, &["check"], None);
    assert_eq!(out.status.code(), Some(2)); // registry has error entries
    let s = stdout_of(&out);
    // herdr-flock HEAD is ae24844b, but the pinned ref resolves to 11111111;
    // pinned.stable matches the ref (up to date) while pinned.old does not.
    let stable: Vec<&str> = s.lines().filter(|l| l.contains("pinned.stable")).collect();
    assert_eq!(stable.len(), 1, "{s}");
    assert!(stable[0].contains("up to date"), "{s}");
    let old: Vec<&str> = s.lines().filter(|l| l.contains("pinned.old")).collect();
    assert_eq!(old.len(), 1, "{s}");
    assert!(old[0].contains("update available"), "{s}");
}

#[test]
fn update_passes_ref_flag_for_pinned_plugins() {
    let dir = setup("update-pinned");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(
        out.status.code(),
        Some(2), // registry has check errors
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = installs(&dir);
    assert!(
        log.contains("ragamo/herdr-flock --ref refs/heads/stable"),
        "pinned reinstall must keep the ref: {log}"
    );
    // pinned.stable matches the pinned ref and must not be reinstalled.
    let flock_installs: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("ragamo/herdr-flock"))
        .collect();
    assert_eq!(flock_installs.len(), 1, "install log: {log}");
}

#[test]
fn update_notifies_after_updates() {
    let dir = setup("update-notify");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry has check errors
    let n = notifications(&dir);
    assert!(
        n.contains("herdr-auto-update")
            && n.contains("2 plugin(s) updated, 3 could not be checked"),
        "notification log: {n}"
    );
}

#[test]
fn update_notify_can_be_disabled() {
    let dir = setup("update-notify-off");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"auto\"\nnotify = false\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry has check errors
    assert_eq!(notifications(&dir), "", "notify=false must not notify");
}

#[test]
fn update_notify_skipped_when_nothing_to_do() {
    let dir = setup("update-notify-idle");
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        "policy = \"auto\"\nexclude = [\"herdr-file-viewer\"]\n",
    )
    .unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry has check errors
                                            // herdr-file-viewer excluded, pinned.old still updated -> notify fires.
    let n = notifications(&dir);
    assert!(
        n.contains("1 plugin(s) updated, 3 could not be checked"),
        "notification log: {n}"
    );
}

#[test]
fn update_only_restricts_install() {
    let dir = setup("update-only");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update", "--only", "herdr-file-viewer"], Some(&cfg));
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
        "--only must skip other plugins: {log}"
    );
}

#[test]
fn update_only_unknown_plugin_is_fatal() {
    let dir = setup("update-only-missing");
    let out = run(&dir, &["update", "--only", "nope"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));
    assert_eq!(installs(&dir), "");
}

#[test]
fn update_only_local_plugin_is_fatal() {
    let dir = setup("update-only-local");
    let out = run(&dir, &["update", "--only", "linked-dev"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a GitHub-installed plugin"));
    assert_eq!(installs(&dir), "");
}

#[test]
fn update_json_does_not_notify() {
    let dir = setup("update-json-notify");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update", "--json"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // registry has check errors
    assert_eq!(notifications(&dir), "", "--json must not notify");
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

/// git stub that hangs until killed; paired with SLOW_REGISTRY.
const STUB_GIT_HANG_SH: &str = "#!/bin/sh\nsleep 30\n";
const STUB_GIT_HANG_CMD: &str = "@echo off\r\nping -n 30 127.0.0.1 >nul\r\nexit /b 0\r\n";

#[test]
fn update_kills_git_past_deadline() {
    let dir = setup("update-timeout");
    write_registry(&dir, SLOW_REGISTRY);
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "timeout_secs = 1\n").unwrap();
    if cfg!(windows) {
        std::fs::write(dir.join("stub-git.cmd"), STUB_GIT_HANG_CMD).unwrap();
    } else {
        std::fs::write(dir.join("stub-git.sh"), STUB_GIT_HANG_SH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("stub-git.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
    let start = std::time::Instant::now();
    let out = run(&dir, &["update"], Some(&cfg));
    let elapsed = start.elapsed();
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("timed out after 1s"), "{e}");
    assert!(
        elapsed.as_secs() < 20,
        "hung git must be killed, not waited for: {elapsed:?}"
    );
    assert_eq!(installs(&dir), "", "timed-out plugin must not be installed");
}

/// git stub that records start/end around a short sleep, so tests can assert
/// that at most `max_concurrency` git processes run at once.
const STUB_GIT_POOL_SH: &str = r#"#!/bin/sh
log="$(dirname "$0")/git-runs.log"
echo "start" >> "$log"
sleep 0.1
echo "end" >> "$log"
url=""
ref=""
for a in "$@"; do
  case "$a" in
    https://github.com/*) url="$a" ;;
    refs/*) ref="$a" ;;
  esac
done
echo "git ${url:-<none>}" >> "$log"
case "$url" in
  *herdr-flock*)
    if [ -n "$ref" ]; then
      echo "1111111111111111111111111111111111111111 HEAD"
    else
      echo "ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD"
    fi
    ;;
  *herdr-file-viewer*) echo "71d4c1c3706e7958c714789b035a99d949620a9e HEAD" ;;
  *) exit 1 ;;
esac
"#;

const STUB_GIT_POOL_CMD: &str = "@echo off\r\n\
echo start>> \"%~dp0git-runs.log\"\r\n\
ping -n 2 127.0.0.1 >nul\r\n\
echo end>> \"%~dp0git-runs.log\"\r\n\
echo git %*>> \"%~dp0git-runs.log\"\r\n\
set \"has_ref=no\"\r\n\
for %%a in (%*) do (\r\n\
  echo %%a | findstr /c:\"refs/\" >nul\r\n\
  if not errorlevel 1 set \"has_ref=yes\"\r\n\
)\r\n\
echo %* | findstr /c:\"herdr-flock\" >nul\r\n\
if not errorlevel 1 (\r\n\
  if \"%has_ref%\"==\"yes\" ( echo 1111111111111111111111111111111111111111 HEAD & exit /b 0 )\r\n\
  echo ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3 HEAD & exit /b 0\r\n\
)\r\n\
echo %* | findstr /c:\"herdr-file-viewer\" >nul\r\n\
if not errorlevel 1 ( echo 71d4c1c3706e7958c714789b035a99d949620a9e HEAD & exit /b 0 )\r\n\
exit /b 1\r\n";

#[test]
fn update_concurrency_bounded_by_config() {
    let dir = setup("update-pool");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "max_concurrency = 1\n").unwrap();
    if cfg!(windows) {
        std::fs::write(dir.join("stub-git.cmd"), STUB_GIT_POOL_CMD).unwrap();
    } else {
        std::fs::write(dir.join("stub-git.sh"), STUB_GIT_POOL_SH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("stub-git.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
    let out = run(&dir, &["update"], Some(&cfg));
    let log = std::fs::read_to_string(dir.join("git-runs.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    assert!(lines.len() >= 4, "git must run more than once: {log}");
    for w in lines.windows(2) {
        assert!(
            !(w[0] == "start" && w[1] == "start"),
            "concurrent git runs with max_concurrency=1: {log}"
        );
    }
    assert_eq!(out.status.code(), Some(2)); // REGISTRY still has check errors
}

#[test]
fn plan_reports_actions_without_installing() {
    let dir = setup("plan");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["plan"], Some(&cfg));
    // REGISTRY has check errors -> unified contract exit 2.
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.contains("herdr-file-viewer"), "{s}");
    assert!(s.contains("action: UPDATE"), "{s}");
    assert!(s.contains("action: HOLD"), "{s}");
    assert!(s.contains("status: behind"), "{s}");
    // Plan never installs.
    assert_eq!(installs(&dir), "", "plan must not reinstall anything");
}

#[test]
fn plan_clean_registry_notify_holds_exits_0() {
    let dir = setup("plan-clean");
    write_registry(&dir, CLEAN_REGISTRY);
    // No config file -> v1.0 default policy notify: upstream changed
    // (file-viewer behind) but nothing would apply -> exit 0 (v1.0.1
    // contract: 1 means "updates would apply", not "upstream moved").
    let out = run(&dir, &["plan"], None);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(installs(&dir), "", "plan must not reinstall anything");
}

#[test]
fn plan_json_includes_status_and_action() {
    let dir = setup("plan-json");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["plan", "--json"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.contains("\"status\": \"behind\""), "{s}");
    assert!(s.contains("\"status\": \"same\""), "{s}");
    assert!(s.contains("\"action\": \"update\""), "{s}");
    assert!(s.contains("\"policy\": \"auto\""), "{s}");
    assert_eq!(
        installs(&dir),
        "",
        "plan --json must not reinstall anything"
    );
}

#[test]
fn check_json_includes_status_field() {
    let dir = setup("check-status-json");
    let out = run(&dir, &["check", "--json"], None);
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.contains("\"status\": \"behind\""), "{s}");
    assert!(s.contains("\"status\": \"same\""), "{s}");
    assert!(s.contains("\"status\": \"unknown\""), "{s}");
}

#[test]
fn policy_notify_never_installs() {
    let dir = setup("policy-notify");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"notify\"\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    // Errors still drive exit 2; notify policy holds every update.
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(installs(&dir), "", "notify policy must not install");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("0 updated"), "{err}");
}

#[test]
fn policy_pinned_only_updates_only_pinned_plugins() {
    let dir = setup("policy-pinned-only");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"pinned-only\"\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // check errors remain
    let log = installs(&dir);
    assert!(
        log.contains("plugin install ragamo/herdr-flock"),
        "pinned plugin must be updated: {log}"
    );
    assert!(
        !log.contains("smarzban/herdr-file-viewer"),
        "unpinned plugin must be held: {log}"
    );
}

#[test]
fn allow_restricts_updates_by_owner_repo() {
    let dir = setup("allow");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"auto\"\nallow = [\"ragamo/*\"]\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2));
    let log = installs(&dir);
    assert!(
        log.contains("plugin install ragamo/herdr-flock"),
        "allowed owner must be updated: {log}"
    );
    assert!(
        !log.contains("smarzban/herdr-file-viewer"),
        "owner outside allow list must be held: {log}"
    );
}

#[test]
fn apply_acts_like_update() {
    let dir = setup("apply");
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["apply"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // check errors
    let log = installs(&dir);
    assert!(log.contains("smarzban/herdr-file-viewer"), "{log}");
    assert!(log.contains("ragamo/herdr-flock"), "{log}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("2 updated, 0 failed"), "{err}");
}

#[test]
fn diverged_is_held_by_default() {
    let dir = setup("diverged-held");
    // Override the compare stub: diverged history (force push) for every repo.
    let stub = dir.join("stub-curl.sh");
    if stub.exists() {
        std::fs::write(&stub, "#!/bin/sh\necho '{\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}'\n").unwrap();
    } else {
        std::fs::write(
            dir.join("stub-curl.cmd"),
            "@echo off\r\necho {\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}\r\nexit /b 0\r\n",
        )
        .unwrap();
    }
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update"], Some(&cfg));
    // REGISTRY has check errors -> 2; herdr-file-viewer must NOT be installed.
    assert_eq!(out.status.code(), Some(2));
    let log = installs(&dir);
    assert!(
        !log.contains("smarzban/herdr-file-viewer"),
        "diverged must not install by default: {log}"
    );
    let s = stdout_of(&out);
    assert!(s.contains("diverged"), "{s}");
}

#[test]
fn diverged_installs_with_allow_force_push() {
    let dir = setup("diverged-force");
    let stub = dir.join("stub-curl.sh");
    if stub.exists() {
        std::fs::write(&stub, "#!/bin/sh\necho '{\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}'\n").unwrap();
    } else {
        std::fs::write(
            dir.join("stub-curl.cmd"),
            "@echo off\r\necho {\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}\r\nexit /b 0\r\n",
        )
        .unwrap();
    }
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"auto\"\nallow_force_push = true\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(2)); // errors still present in REGISTRY
    assert!(installs(&dir).contains("smarzban/herdr-file-viewer"));
}

#[test]
fn rate_limit_degrades_to_unknown() {
    let dir = setup("rate-limit");
    // curl exits 1 (rate limit / network) -> classify fails -> Unknown.
    let stub = dir.join("stub-curl.sh");
    if stub.exists() {
        std::fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
    } else {
        std::fs::write(dir.join("stub-curl.cmd"), "@echo off\r\nexit /b 1\r\n").unwrap();
    }
    let out = run(&dir, &["check", "--json"], None);
    assert_eq!(out.status.code(), Some(2));
    let s = stdout_of(&out);
    assert!(s.contains("\"status\": \"unknown\""), "{s}");
    assert!(s.contains("request failed"), "{s}");
}

#[test]
fn rollback_reinstalls_previous_commit() {
    let dir = setup("rollback");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let prev = "350f3f5be79d136933ba36c8c8dd60f79df28002";
    let cur = "71d4c1c3706e7958c714789b035a99d949620a9e";
    std::fs::write(
        dir.join("state.json"),
        format!(
            r#"{{"version":1,"entries":[{{"plugin_id":"herdr-file-viewer","previous_sha":"{prev}","current_sha":"{cur}","updated_at":"2026-08-15T00:00:00Z","result":"updated"}}]}}"#
        ),
    )
    .unwrap();
    let out = run(&dir, &["rollback"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let log = installs(&dir);
    assert!(
        log.contains(&format!("smarzban/herdr-file-viewer --ref {prev} --yes")),
        "rollback must reinstall the previous commit: {log}"
    );
    let s = stdout_of(&out);
    assert!(s.contains("rolling back"), "{s}");
}

#[test]
fn rollback_rewinds_only_latest_update_per_plugin() {
    let dir = setup("rollback-multi");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let a = "350f3f5be79d136933ba36c8c8dd60f79df28002";
    let b = "71d4c1c3706e7958c714789b035a99d949620a9e";
    let c = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    std::fs::write(
        dir.join("state.json"),
        format!(
            r#"{{"version":1,"entries":[
{{"plugin_id":"herdr-file-viewer","previous_sha":"{a}","current_sha":"{b}","updated_at":"2026-08-15T00:00:00Z","result":"updated"}},
{{"plugin_id":"herdr-file-viewer","previous_sha":"{b}","current_sha":"{c}","updated_at":"2026-08-15T00:01:00Z","result":"updated"}}
]}}"#
        ),
    )
    .unwrap();
    let out = run(&dir, &["rollback"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let log = installs(&dir);
    assert!(
        log.contains(&format!("smarzban/herdr-file-viewer --ref {b} --yes")),
        "rollback must install the commit before the LATEST update: {log}"
    );
    assert!(
        !log.contains(&format!("--ref {a}")),
        "rollback must not rewind older history: {log}"
    );
    let st = std::fs::read_to_string(dir.join("state.json")).unwrap();
    assert!(
        st.contains("\"result\": \"rolled_back\""),
        "rollback must be recorded in state.json: {st}"
    );
    assert!(
        st.contains(&format!("\"current_sha\": \"{b}\"")),
        "rolled_back entry must point at the restored commit: {st}"
    );
}

#[test]
fn rollback_skips_plugin_already_rolled_back() {
    let dir = setup("rollback-already");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let a = "350f3f5be79d136933ba36c8c8dd60f79df28002";
    let b = "71d4c1c3706e7958c714789b035a99d949620a9e";
    std::fs::write(
        dir.join("state.json"),
        format!(
            r#"{{"version":1,"entries":[
{{"plugin_id":"herdr-file-viewer","previous_sha":"{a}","current_sha":"{b}","updated_at":"2026-08-15T00:00:00Z","result":"updated"}},
{{"plugin_id":"herdr-file-viewer","previous_sha":"{b}","current_sha":"{a}","updated_at":"2026-08-15T00:01:00Z","result":"rolled_back"}}
]}}"#
        ),
    )
    .unwrap();
    let out = run(&dir, &["rollback"], Some(&cfg));
    assert_eq!(out.status.code(), Some(1)); // nothing to roll back
    assert_eq!(
        installs(&dir),
        "",
        "already rolled back must not reinstall anything"
    );
}

#[test]
fn resume_reinstalls_original_tracking_ref() {
    let dir = setup("resume");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let a = "350f3f5be79d136933ba36c8c8dd60f79df28002";
    let b = "71d4c1c3706e7958c714789b035a99d949620a9e";
    std::fs::write(
        dir.join("state.json"),
        format!(
            r#"{{"version":1,"entries":[
{{"plugin_id":"herdr-file-viewer","previous_sha":"{a}","current_sha":"{b}","ref":"main","updated_at":"2026-08-15T00:00:00Z","result":"updated"}},
{{"plugin_id":"herdr-file-viewer","previous_sha":"{b}","current_sha":"{a}","ref":"main","updated_at":"2026-08-15T00:01:00Z","result":"rolled_back"}}
]}}"#
        ),
    )
    .unwrap();
    let out = run(&dir, &["resume"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let log = installs(&dir);
    assert!(
        log.contains("smarzban/herdr-file-viewer --ref main --yes"),
        "resume must reinstall with the original tracking ref: {log}"
    );
    let st = std::fs::read_to_string(dir.join("state.json")).unwrap();
    assert!(
        st.contains("\"result\": \"updated\""),
        "resume must record the rejoin in state.json: {st}"
    );
    let s = stdout_of(&out);
    assert!(s.contains("resuming tracking ref main"), "{s}");
}

#[test]
fn resume_default_branch_installs_without_ref() {
    let dir = setup("resume-default");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    let a = "350f3f5be79d136933ba36c8c8dd60f79df28002";
    let b = "71d4c1c3706e7958c714789b035a99d949620a9e";
    std::fs::write(
        dir.join("state.json"),
        format!(
            r#"{{"version":1,"entries":[
{{"plugin_id":"herdr-file-viewer","previous_sha":"{a}","current_sha":"{b}","updated_at":"2026-08-15T00:00:00Z","result":"updated"}},
{{"plugin_id":"herdr-file-viewer","previous_sha":"{b}","current_sha":"{a}","updated_at":"2026-08-15T00:01:00Z","result":"rolled_back"}}
]}}"#
        ),
    )
    .unwrap();
    let out = run(&dir, &["resume"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let log = installs(&dir);
    assert!(
        log.contains("smarzban/herdr-file-viewer --yes"),
        "resume of a default-branch tracker must install without --ref: {log}"
    );
    assert!(!log.contains("--ref"), "{log}");
}

#[test]
fn rollback_only_unknown_with_empty_state_is_fatal() {
    let dir = setup("rollback-only-empty");
    // No state.json: the --only path must still error (never silently no-op).
    let out = run(&dir, &["rollback", "--only", "nope"], None);
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("no update recorded for plugin 'nope'"), "{e}");
    assert_eq!(installs(&dir), "");
}

#[test]
fn resume_without_state_exits_0() {
    let dir = setup("resume-empty");
    let out = run(&dir, &["resume"], None);
    assert_eq!(out.status.code(), Some(0)); // nothing to resume is not a failure
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("no rollbacks recorded"), "{e}");
    assert_eq!(installs(&dir), "");
}

/// PINNED_REGISTRY with commit.pinned's requested_ref cleared (what herdr's
/// registry looks like after a ref-less reinstall) — used by the untrack
/// stub to confirm the pin is gone post-install.
const PINNED_REGISTRY_CLEARED: &str = r#"{"id":"cli:plugin","result":{"plugins":[
  {"plugin_id":"tag.pinned","version":"1.0.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"2222222222222222222222222222222222222222","requested_ref":"refs/tags/v1.0.0"}},
  {"plugin_id":"commit.pinned","version":"1.0.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"3333333333333333333333333333333333333333"}},
  {"plugin_id":"flock.farm","version":"0.1.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3"}}
]}}"#;

/// herdr stub that records installs and, after the first install, reports the
/// cleared registry (ref-less install semantics).
fn write_untrack_stub(dir: &Path) {
    if cfg!(windows) {
        std::fs::write(
            dir.join("stub-herdr.cmd"),
            "@echo off\r\n\
if \"%1\"==\"plugin\" if \"%2\"==\"list\" (\r\n\
  if exist \"%~dp0installed.flag\" ( type \"%~dp0registry-cleared.json\" ) else ( type \"%~dp0registry.json\" )\r\n\
  exit /b 0\r\n\
)\r\n\
if \"%1\"==\"plugin\" if \"%2\"==\"install\" (\r\n\
  echo %*>> \"%~dp0installs.log\"\r\n\
  type nul > \"%~dp0installed.flag\"\r\n\
  exit /b 0\r\n\
)\r\n\
exit /b 1\r\n",
        )
        .unwrap();
    } else {
        std::fs::write(
            dir.join("stub-herdr.sh"),
            "#!/bin/sh\n\
if [ \"$1\" = \"plugin\" ] && [ \"$2\" = \"list\" ]; then\n\
  if [ -f \"$(dirname \"$0\")/installed.flag\" ]; then cat \"$(dirname \"$0\")/registry-cleared.json\"; else cat \"$(dirname \"$0\")/registry.json\"; fi\n\
  exit 0\n\
fi\n\
if [ \"$1\" = \"plugin\" ] && [ \"$2\" = \"install\" ]; then\n\
  echo \"$@\" >> \"$(dirname \"$0\")/installs.log\"\n\
  touch \"$(dirname \"$0\")/installed.flag\"\n\
  exit 0\n\
fi\n\
exit 1\n",
        )
        .unwrap();
    }
}

#[test]
fn untrack_requires_only() {
    let dir = setup("untrack-no-only");
    write_registry(&dir, PINNED_REGISTRY);
    let out = run(&dir, &["untrack"], None);
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("untrack requires --only"), "{e}");
    assert_eq!(installs(&dir), "");
}

#[test]
fn untrack_reinstalls_commit_pin_ref_less() {
    let dir = setup("untrack");
    write_registry(&dir, PINNED_REGISTRY);
    std::fs::write(dir.join("registry-cleared.json"), PINNED_REGISTRY_CLEARED).unwrap();
    write_untrack_stub(&dir);
    let out = run(&dir, &["untrack", "--only", "commit.pinned"], None);
    assert_eq!(out.status.code(), Some(0));
    let log = installs(&dir);
    assert!(
        log.contains("ragamo/herdr-flock --yes"),
        "untrack must reinstall without --ref: {log}"
    );
    assert!(!log.contains("--ref"), "untrack must not pass --ref: {log}");
    let s = stdout_of(&out);
    assert!(s.contains("now tracking the default branch"), "{s}");
}

#[test]
fn untrack_rejects_non_pinned_and_unknown() {
    // Branch-tracked plugin -> rejected.
    let dir = setup("untrack-branch");
    write_registry(&dir, PINNED_REGISTRY);
    let out = run(&dir, &["untrack", "--only", "flock.farm"], None);
    assert_eq!(out.status.code(), Some(2));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("not pinned to a commit"), "{e}");
    assert_eq!(installs(&dir), "");
    // Unknown id -> rejected.
    let dir2 = setup("untrack-missing");
    write_registry(&dir2, PINNED_REGISTRY);
    let out2 = run(&dir2, &["untrack", "--only", "nope"], None);
    assert_eq!(out2.status.code(), Some(2));
    assert_eq!(installs(&dir2), "");
}

#[test]
fn update_reports_commit_pins_and_notifies() {
    // NOTE: unique setup name - update_passes_ref_flag_for_pinned_plugins
    // also uses "update-pinned"; sharing a temp dir races the two tests.
    let dir = setup("update-pinned-notify");
    write_registry(&dir, PINNED_REGISTRY);
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap(); // default notify policy
    let out = run(&dir, &["update", "--json"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let s = stdout_of(&out);
    assert!(
        s.contains("\"pinned\": [\n    \"commit.pinned\"\n  ]"),
        "update --json must list commit-pinned plugins: {s}"
    );
    assert!(s.contains("\"held\": [\n    \"tag.pinned\"\n  ]"), "{s}");
    // Human run: notice on stderr + desktop notification body.
    let out2 = run(&dir, &["update"], Some(&cfg));
    let e = String::from_utf8_lossy(&out2.stderr);
    assert!(
        e.contains("pinned to commits and cannot be auto-updated"),
        "{e}"
    );
    assert!(e.contains("untrack --only"), "{e}");
    let n = notifications(&dir);
    assert!(n.contains("pinned to commits"), "{n}");
    // Non-interactive (test stdin is not a TTY): pinned plugins must NOT be
    // reinstalled; the interactive prompt is TTY-gated.
    assert_eq!(
        installs(&dir),
        "",
        "non-interactive update must not reinstall pinned plugins"
    );
}

#[test]
fn plan_exit_reflects_actions_not_raw_changes() {
    // notify policy: upstream changed (behind) but action is HOLD -> 0.
    let dir = setup("plan-exit-notify");
    write_registry(&dir, CLEAN_REGISTRY);
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"notify\"\n").unwrap();
    let out = run(&dir, &["plan"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0), "notify+HOLD must exit 0");
    let s = stdout_of(&out);
    assert!(s.contains("action: HOLD"), "{s}");

    // auto policy: behind -> UPDATE -> 1.
    let dir2 = setup("plan-exit-auto");
    write_registry(&dir2, CLEAN_REGISTRY);
    let cfg2 = auto_cfg(&dir2, "");
    let out2 = run(&dir2, &["plan"], Some(&cfg2));
    assert_eq!(out2.status.code(), Some(1), "auto+UPDATE must exit 1");
    let s2 = stdout_of(&out2);
    assert!(s2.contains("action: UPDATE"), "{s2}");

    // diverged + allow_force_push -> UPDATE even though update_available is
    // false for those plugins -> 1 (contract: 1 = updates would apply).
    let dir3 = setup("plan-exit-force");
    write_registry(&dir3, CLEAN_REGISTRY);
    let stub = dir3.join("stub-curl.sh");
    if stub.exists() {
        std::fs::write(&stub, "#!/bin/sh\necho '{\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}'\n").unwrap();
    } else {
        std::fs::write(
            dir3.join("stub-curl.cmd"),
            "@echo off\r\necho {\"status\":\"diverged\",\"ahead_by\":1,\"behind_by\":1,\"total_commits\":2}\r\nexit /b 0\r\n",
        )
        .unwrap();
    }
    let cfg3 = dir3.join("config.toml");
    std::fs::write(&cfg3, "policy = \"auto\"\nallow_force_push = true\n").unwrap();
    let out3 = run(&dir3, &["plan"], Some(&cfg3));
    assert_eq!(out3.status.code(), Some(1), "force-push UPDATE must exit 1");
    let s3 = stdout_of(&out3);
    assert!(s3.contains("action: UPDATE"), "{s3}");
}

#[test]
fn update_records_state_history() {
    let dir = setup("state-record");
    write_registry(&dir, CLEAN_REGISTRY);
    let cfg = auto_cfg(&dir, "");
    let out = run(&dir, &["update"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0)); // no errors in CLEAN_REGISTRY
    let st = std::fs::read_to_string(dir.join("state.json")).unwrap_or_default();
    assert!(st.contains("\"herdr-file-viewer\""), "{st}");
    assert!(st.contains("\"result\": \"updated\""), "{st}");
    assert!(
        st.contains("\"previous_sha\": \"350f3f5be79d136933ba36c8c8dd60f79df28002\""),
        "{st}"
    );
}

#[test]
fn history_prints_recorded_updates() {
    let dir = setup("history");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap();
    std::fs::write(
        dir.join("state.json"),
        r#"{"version":1,"entries":[{"plugin_id":"herdr-file-viewer","previous_sha":"350f3f5be79d136933ba36c8c8dd60f79df28002","current_sha":"71d4c1c3706e7958c714789b035a99d949620a9e","updated_at":"2026-08-15T00:00:00Z","result":"updated"}]}"#,
    )
    .unwrap();
    let out = run(&dir, &["history"], Some(&cfg));
    assert_eq!(out.status.code(), Some(0));
    let s = stdout_of(&out);
    assert!(s.contains("updated herdr-file-viewer"), "{s}");
}

/// Registry with a tag-pinned and a commit-pinned plugin, no error entries.
const PINNED_REGISTRY: &str = r#"{"id":"cli:plugin","result":{"plugins":[
  {"plugin_id":"tag.pinned","version":"1.0.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"2222222222222222222222222222222222222222","requested_ref":"refs/tags/v1.0.0"}},
  {"plugin_id":"commit.pinned","version":"1.0.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"3333333333333333333333333333333333333333","requested_ref":"3333333333333333333333333333333333333333"}},
  {"plugin_id":"flock.farm","version":"0.1.0","source":{"kind":"github","owner":"ragamo","repo":"herdr-flock","resolved_commit":"ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3"}}
]}}"#;

/// v0.4: the pin channel (branch / tag / commit) is surfaced per plugin.
#[test]
fn plan_reports_ref_channel() {
    let dir = setup("plan-channel");
    write_registry(&dir, PINNED_REGISTRY);
    // Default notify policy: every pin is held, nothing would apply -> 0
    // (v1.0.1 contract; upstream moved but no UPDATE action exists).
    let out = run(&dir, &["plan"], None);
    assert_eq!(out.status.code(), Some(0));
    let s = stdout_of(&out);
    let block: Vec<&str> = s.lines().collect();
    for (name, want) in [
        ("tag.pinned", "channel: tag"),
        ("commit.pinned", "channel: commit"),
        ("flock.farm", "channel: branch"),
    ] {
        let idx = block
            .iter()
            .position(|l| l.contains(name))
            .unwrap_or_else(|| panic!("{name} missing: {s}"));
        assert!(
            block[idx..].iter().take(6).any(|l| l.contains(want)),
            "{name} should show '{want}': {s}"
        );
    }
}

/// v0.4: a commit pin IS the installed commit - never a network check, never
/// an update candidate.
#[test]
fn commit_pin_skips_network_and_never_updates() {
    let dir = setup("commit-pin");
    write_registry(&dir, PINNED_REGISTRY);
    // The logging stub records every git invocation so the test can assert
    // the commit-pinned plugin never reaches the network.
    if cfg!(windows) {
        std::fs::write(dir.join("stub-git.cmd"), STUB_GIT_POOL_CMD).unwrap();
    } else {
        std::fs::write(dir.join("stub-git.sh"), STUB_GIT_POOL_SH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("stub-git.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
    // Isolate the commit-pinned plugin: a check restricted to it must not
    // spawn a single git process (the pin IS the installed commit). Counting
    // absolute runs on the full registry is brittle under parallel test
    // runners, so the isolation proof is scoped with --only.
    let out = run(&dir, &["check", "--only", "commit.pinned"], None);
    assert_eq!(out.status.code(), Some(0)); // no changes, no errors
    let log = std::fs::read_to_string(dir.join("git-runs.log")).unwrap_or_default();
    let calls: Vec<&str> = log.lines().filter(|l| *l == "start").collect();
    assert_eq!(calls.len(), 0, "commit.pinned must not hit git: {log}");
    let s = stdout_of(&out);
    assert!(s.contains("commit.pinned"), "{s}");
    assert!(s.contains("up to date"), "{s}");
}

#[test]
fn trusted_owners_holds_untrusted_owners() {
    let dir = setup("trusted-owners");
    write_registry(&dir, CLEAN_REGISTRY);
    let cfg = dir.join("config.toml");
    // herdr-file-viewer is smarzban/...; only ragamo is trusted.
    std::fs::write(&cfg, "policy = \"auto\"\ntrusted_owners = [\"ragamo\"]\n").unwrap();
    let out = run(&dir, &["update"], Some(&cfg));
    // CLEAN_REGISTRY has 2 updates (file-viewer, pinned.old); file-viewer held.
    let log = installs(&dir);
    assert!(
        !log.contains("smarzban"),
        "untrusted owner must not install: {log}"
    );
    assert!(
        log.contains("ragamo/herdr-flock"),
        "trusted owner may install: {log}"
    );
    let s = stdout_of(&out);
    assert!(s.contains("owner 'smarzban' not trusted"), "{s}");
}

#[test]
fn immutable_pins_hold_tag_and_commit() {
    let dir = setup("immutable-pins");
    write_registry(&dir, PINNED_REGISTRY);
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "immutable_pins = true\n").unwrap();
    let out = run(&dir, &["plan"], Some(&cfg));
    let s = stdout_of(&out);
    assert!(s.contains("tag.pinned"), "{s}");
    assert!(s.contains("immutable pin (tag)"), "{s}");
    assert!(s.contains("commit.pinned"), "{s}");
    // commit.pinned is same-status (pin IS the commit) -> HOLD via up-to-date.
    let lines: Vec<&str> = s.lines().collect();
    let cidx = lines
        .iter()
        .position(|l| l.contains("commit.pinned"))
        .unwrap();
    let cblock = lines[cidx..]
        .iter()
        .take(8)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(cblock.contains("action: HOLD"), "{cblock}");
    assert!(!cblock.contains("action: UPDATE"), "{cblock}");
    // flock.farm (branch/default) is not held by the pin rule.
    let fidx = lines.iter().position(|l| l.contains("flock.farm")).unwrap();
    let fblock = lines[fidx..]
        .iter()
        .take(8)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!fblock.contains("immutable pin"), "{fblock}");
}

#[test]
fn commit_pin_is_never_updateable() {
    let dir = setup("commit-pin-immutable-off");
    write_registry(&dir, PINNED_REGISTRY);
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "policy = \"auto\"\nimmutable_pins = false\n").unwrap();
    let out = run(&dir, &["plan"], Some(&cfg));
    let s = stdout_of(&out);
    // commit.pinned: pin IS the commit -> same, never UPDATE even without immutable_pins.
    let block: Vec<&str> = s.lines().collect();
    let idx = block
        .iter()
        .position(|l| l.contains("commit.pinned"))
        .unwrap();
    let joined: Vec<&str> = block[idx..].iter().take(8).copied().collect();
    let joined = joined.join("\n");
    assert!(joined.contains("status: same"), "{joined}");
    assert!(joined.contains("action: HOLD"), "{joined}");
}

#[test]
fn standalone_resolves_plugin_config_dir_via_herdr() {
    // A standalone run (no HERDR_PLUGIN_CONFIG_DIR, no --config) must find
    // the plugin config through `herdr plugin config-dir` and honor it -
    // this is what makes `policy = "auto"` work when the user runs
    // `herdr-auto-update update` directly from a shell.
    let dir = setup("standalone-config");
    std::fs::create_dir_all(dir.join("plugin-config")).unwrap();
    std::fs::write(dir.join("plugin-config/config.toml"), "policy = \"auto\"\n").unwrap();
    let out = run(&dir, &["update"], None);
    let log = installs(&dir);
    assert!(
        log.contains("smarzban/herdr-file-viewer"),
        "policy=auto from the plugin config dir must install: {log}"
    );
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        !e.contains("policy` is not set"),
        "config with explicit policy must not warn: {e}"
    );
}

#[test]
fn unset_policy_emits_migration_warning() {
    let dir = setup("migration-warning");
    let cfg = dir.join("config.toml");
    std::fs::write(&cfg, "").unwrap(); // policy unset -> v1.0 default notify
    let out = run(&dir, &["update"], Some(&cfg));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.contains("policy` is not set; defaulting to \"notify\""),
        "{e}"
    );
    // Explicit policy silences the warning.
    let cfg2 = dir.join("config2.toml");
    std::fs::write(&cfg2, "policy = \"notify\"\n").unwrap();
    let out2 = run(&dir, &["update"], Some(&cfg2));
    let e2 = String::from_utf8_lossy(&out2.stderr);
    assert!(!e2.contains("policy` is not set"), "{e2}");
    // Explicit auto policy also silences it (opt-in).
    let cfg3 = dir.join("config3.toml");
    std::fs::write(&cfg3, "policy = \"auto\"\n").unwrap();
    let out3 = run(&dir, &["update"], Some(&cfg3));
    let e3 = String::from_utf8_lossy(&out3.stderr);
    assert!(!e3.contains("policy` is not set"), "{e3}");
}
