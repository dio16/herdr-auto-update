// Core logic: compare each installed github plugin against its upstream HEAD
// via `git ls-remote`, then reinstall outdated ones through the herdr CLI.
// All subprocess calls use argv arrays - no shell interpolation anywhere.

use crate::config::Config;
use crate::registry;
use serde::Serialize;
use std::io::Read;
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Short network timeouts so a dead connection cannot stall herdr startup.
/// These are git-config hints; a hard wall-clock kill is enforced separately
/// by `run_git_with_timeout`, so a wedged connection cannot hang forever.
/// (`http.connectTimeout` is not a real git-config key, hence absent.)
const GIT_TIMEOUT_ARGS: [&str; 4] = ["-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=15"];

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub plugin_id: String,
    pub owner: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub installed_sha: String,
    pub remote_sha: Option<String>,
    pub update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn run_startup(cfg: &Config, json: bool) -> ExitCode {
    if !cfg.auto_update {
        if !json {
            eprintln!("[herdr-auto-update] auto_update disabled; skipping startup check");
        }
        return ExitCode::SUCCESS;
    }
    run_update(cfg, json, None)
}

pub fn run_check(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let statuses = match collect(cfg, only) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let pending = statuses.iter().filter(|s| s.update_available).count();
    let errors = statuses.iter().filter(|s| s.error.is_some()).count();
    if json {
        println!("{}", serde_json::to_string_pretty(&statuses).unwrap());
    } else {
        for s in &statuses {
            print_status(s);
        }
        eprintln!(
            "[herdr-auto-update] {} plugin(s) checked, {} update(s) available, {} error(s)",
            statuses.len(),
            pending,
            errors
        );
    }
    // Errors take precedence over pending updates so scripts can tell a
    // failed check (2) apart from "updates available" (1).
    if errors > 0 {
        ExitCode::from(2)
    } else if pending > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub fn run_update(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let statuses = match collect(cfg, only) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let mut updated: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for s in &statuses {
        if let Some(err) = &s.error {
            if !json {
                eprintln!("  [{}] error: {err}", s.plugin_id);
            }
            errors.push(s.plugin_id.clone());
            continue;
        }
        if !s.update_available {
            if !json {
                println!(
                    "  [{}] up to date ({})",
                    s.plugin_id,
                    short(&s.installed_sha)
                );
            }
            continue;
        }
        if cfg.is_excluded(&s.plugin_id) {
            excluded.push(s.plugin_id.clone());
            if !json {
                println!(
                    "  [{}] update available ({}) but excluded",
                    s.plugin_id,
                    short(&s.installed_sha)
                );
            }
            continue;
        }
        if !json {
            let pin = s
                .requested_ref
                .as_deref()
                .map(|r| format!(" (pinned {r})"))
                .unwrap_or_default();
            println!(
                "  [{}] updating {} -> {}{}",
                s.plugin_id,
                short(&s.installed_sha),
                short(s.remote_sha.as_deref().unwrap_or("?")),
                pin
            );
        }
        if apply_update(&s.owner, &s.repo, s.requested_ref.as_deref()) {
            updated.push(s.plugin_id.clone());
        } else {
            failed.push(s.plugin_id.clone());
        }
    }

    if json {
        let report = serde_json::json!({
            "updated": updated,
            "failed": failed,
            "excluded": excluded,
            "errors": errors,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        let mut summary = format!(
            "[herdr-auto-update] {} updated, {} failed",
            updated.len(),
            failed.len()
        );
        if !errors.is_empty() {
            summary.push_str(&format!(", {} error(s)", errors.len()));
        }
        eprintln!("{summary}");
        notify(cfg, updated.len(), failed.len(), errors.len());
    }

    // Check errors take precedence over install failures so scripts can tell
    // "could not verify upstream" (2) apart from "installs failed" (1).
    if !errors.is_empty() {
        ExitCode::from(2)
    } else if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Best-effort desktop notification through herdr's CLI. Failures are
/// ignored: a missing notification API must not change the exit code.
fn notify(cfg: &Config, updated: usize, failed: usize, errors: usize) {
    if !cfg.notify || (updated == 0 && failed == 0 && errors == 0) {
        return;
    }
    let body = match (updated, failed, errors) {
        (0, 0, e) => format!("{e} plugin(s) could not be checked"),
        (0, f, 0) => format!("{f} plugin update(s) failed"),
        (0, f, e) => format!("{f} plugin update(s) failed, {e} could not be checked"),
        (u, 0, 0) => format!("{u} plugin(s) updated"),
        (u, 0, e) => format!("{u} plugin(s) updated, {e} could not be checked"),
        (u, f, 0) => format!("{u} updated, {f} failed"),
        (u, f, e) => format!("{u} updated, {f} failed, {e} could not be checked"),
    };
    let bin = registry::herdr_bin();
    let _ = Command::new(&bin)
        .args(["notification", "show", "herdr-auto-update", "--body", &body])
        .status();
}

/// Reinstall one plugin through herdr's own installer (herdr v1 has no
/// dedicated `plugin update`; reinstall replaces the managed checkout while
/// preserving plugin config/state). A plugin pinned with `--ref` at install
/// time is reinstalled against the same ref, not the default branch.
fn apply_update(owner: &str, repo: &str, requested_ref: Option<&str>) -> bool {
    let bin = registry::herdr_bin();
    let spec = format!("{owner}/{repo}");
    let mut cmd = Command::new(&bin);
    cmd.arg("plugin").arg("install").arg(&spec);
    if let Some(r) = requested_ref {
        cmd.args(["--ref", r]);
    }
    match cmd.arg("--yes").status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("  install for {spec} failed with {s}");
            false
        }
        Err(e) => {
            eprintln!("  cannot run '{bin}': {e}");
            false
        }
    }
}

/// A plugin whose remote commit still needs resolving (network call).
struct RemoteJob {
    index: usize,
    plugin_id: String,
    owner: String,
    repo: String,
    version: Option<String>,
    installed_sha: String,
    requested_ref: Option<String>,
}

/// With `only`, restrict to one plugin id; an id that is absent from the
/// registry, or not a GitHub-installed plugin, is a hard error so `--only`
/// can never silently no-op.
fn collect(cfg: &Config, only: Option<&str>) -> Result<Vec<PluginStatus>, String> {
    let plugins = registry::list_installed()?;

    // Resolve each plugin to either an immediate status (skipped kinds,
    // invalid entries) or a remote-resolution job. The remote checks run in
    // parallel below so a slow network cannot multiply across plugins.
    let mut statuses: Vec<Option<PluginStatus>> = Vec::with_capacity(plugins.len());
    let mut jobs: Vec<RemoteJob> = Vec::new();
    let mut matched = false;

    for (i, p) in plugins.iter().enumerate() {
        if let Some(o) = only {
            if p.plugin_id != o {
                statuses.push(None);
                continue;
            }
            matched = true;
        }
        let Some(src) = &p.source else {
            if let Some(o) = only {
                return Err(format!("plugin '{o}' is not a GitHub-installed plugin"));
            }
            statuses.push(None);
            continue;
        };
        if src.kind != registry::GITHUB_KIND {
            if let Some(o) = only {
                return Err(format!("plugin '{o}' is not a GitHub-installed plugin"));
            }
            statuses.push(None);
            continue; // local links and other kinds are not updatable
        }
        // A github entry may be missing owner/repo/resolved_commit (e.g. a
        // stale registry row); treat it as invalid instead of panicking.
        let (Some(owner), Some(repo), Some(rc)) = (&src.owner, &src.repo, &src.resolved_commit)
        else {
            statuses.push(Some(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: src.owner.clone().unwrap_or_default(),
                repo: src.repo.clone().unwrap_or_default(),
                version: p.version.clone(),
                installed_sha: src.resolved_commit.clone().unwrap_or_default(),
                remote_sha: None,
                update_available: false,
                requested_ref: src.requested_ref.clone(),
                error: Some("github source missing owner/repo/commit fields".to_string()),
            }));
            continue;
        };
        if !registry::valid_github_name(owner, 39) || !registry::valid_github_name(repo, 100) {
            statuses.push(Some(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                version: p.version.clone(),
                installed_sha: rc.clone(),
                remote_sha: None,
                update_available: false,
                requested_ref: src.requested_ref.clone(),
                error: Some("invalid owner/repo recorded in registry".to_string()),
            }));
            continue;
        }
        jobs.push(RemoteJob {
            index: i,
            plugin_id: p.plugin_id.clone(),
            owner: owner.clone(),
            repo: repo.clone(),
            version: p.version.clone(),
            installed_sha: rc.clone(),
            requested_ref: src.requested_ref.clone(),
        });
        statuses.push(None);
    }
    if let Some(o) = only {
        if !matched {
            return Err(format!("plugin '{o}' not found in the herdr registry"));
        }
    }

    // Worker pool: at most `max_concurrency` git processes at once. A
    // registry full of plugins must not spawn an unbounded number of
    // processes, and a slow network must not serialize all checks.
    let workers = jobs.len().min(cfg.max_concurrency.max(1));
    let timeout_secs = cfg.timeout_secs;
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let next = &next;
        let jobs = &jobs;
        let (res_tx, res_rx) = mpsc::channel::<(usize, Result<Option<String>, String>)>();
        for _ in 0..workers {
            let res_tx = res_tx.clone();
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= jobs.len() {
                        break;
                    }
                    let job = &jobs[i];
                    let result = remote_head(
                        &job.owner,
                        &job.repo,
                        job.requested_ref.as_deref(),
                        timeout_secs,
                    );
                    // Workers only exit after every job index is claimed, and
                    // the collector only exits after every result arrives.
                    res_tx.send((i, result)).expect("result channel closed");
                }
            });
        }
        drop(res_tx);
        for _ in 0..jobs.len() {
            let (index, result) = res_rx.recv().expect("all workers exited early");
            let job = &jobs[index];
            let status = match result {
                Ok(Some(sha)) => PluginStatus {
                    plugin_id: job.plugin_id.clone(),
                    owner: job.owner.clone(),
                    repo: job.repo.clone(),
                    version: job.version.clone(),
                    installed_sha: job.installed_sha.clone(),
                    remote_sha: Some(sha.clone()),
                    update_available: sha != job.installed_sha,
                    requested_ref: job.requested_ref.clone(),
                    error: None,
                },
                Ok(None) => PluginStatus {
                    plugin_id: job.plugin_id.clone(),
                    owner: job.owner.clone(),
                    repo: job.repo.clone(),
                    version: job.version.clone(),
                    installed_sha: job.installed_sha.clone(),
                    remote_sha: None,
                    update_available: false,
                    requested_ref: job.requested_ref.clone(),
                    error: Some("cannot resolve remote HEAD (repo moved or deleted?)".to_string()),
                },
                Err(e) => PluginStatus {
                    plugin_id: job.plugin_id.clone(),
                    owner: job.owner.clone(),
                    repo: job.repo.clone(),
                    version: job.version.clone(),
                    installed_sha: job.installed_sha.clone(),
                    remote_sha: None,
                    update_available: false,
                    requested_ref: job.requested_ref.clone(),
                    error: Some(e),
                },
            };
            statuses[job.index] = Some(status);
        }
    });

    Ok(statuses.into_iter().flatten().collect())
}

/// Resolve the remote commit for a plugin: its pinned ref when
/// `requested_ref` is set (plugins installed with `--ref`), otherwise the
/// default branch HEAD.
fn remote_head(
    owner: &str,
    repo: &str,
    requested_ref: Option<&str>,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let url = format!("https://github.com/{owner}/{repo}");
    let mut args: Vec<&str> = GIT_TIMEOUT_ARGS.to_vec();
    args.push("ls-remote");
    args.push(&url);
    args.push(requested_ref.unwrap_or("HEAD"));
    let out = run_git_with_timeout(&args, timeout_secs)?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let sha = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(Some(sha.to_owned()))
    } else {
        Ok(None)
    }
}

/// Run `git` with a wall-clock deadline. `timeout_secs == 0` disables the
/// deadline. The child's stdout/stderr are read to completion so the pipe
/// buffer cannot fill up and stall the child past the deadline; a timed-out
/// process is killed and reaped before we return.
fn run_git_with_timeout(args: &[&str], timeout_secs: u64) -> Result<Output, String> {
    let mut child = Command::new(git_bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if timeout_secs == 0 {
        return child
            .wait_with_output()
            .map_err(|e| format!("cannot run git: {e}"));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = stdout.read_to_end(&mut out);
                let _ = stderr.read_to_end(&mut err);
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("git ls-remote timed out after {timeout_secs}s"));
                }
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot wait for git: {e}"));
            }
        }
        // Brief sleep so a hung child does not busy-spin the worker.
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// git binary; HERDR_AUTO_UPDATE_GIT overrides it (used by the test suite to
/// stub network access).
fn git_bin() -> String {
    std::env::var("HERDR_AUTO_UPDATE_GIT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "git".to_string())
}

fn print_status(s: &PluginStatus) {
    if let Some(err) = &s.error {
        println!("  [{}] error: {err}", s.plugin_id);
    } else if s.update_available {
        println!(
            "  [{}] update available: {} -> {}",
            s.plugin_id,
            short(&s.installed_sha),
            short(s.remote_sha.as_deref().unwrap_or("?"))
        );
    } else {
        println!(
            "  [{}] up to date ({})",
            s.plugin_id,
            short(&s.installed_sha)
        );
    }
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_truncates() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }

    #[test]
    fn git_bin_env_override() {
        std::env::set_var("HERDR_AUTO_UPDATE_GIT", "stub-git");
        assert_eq!(git_bin(), "stub-git");
        std::env::remove_var("HERDR_AUTO_UPDATE_GIT");
        assert_eq!(git_bin(), "git");
    }
}
