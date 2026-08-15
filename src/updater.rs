// Core logic: compare each installed github plugin against its upstream HEAD
// via `git ls-remote`, then reinstall outdated ones through the herdr CLI.
// All subprocess calls use argv arrays - no shell interpolation anywhere.

use crate::config::{Config, Policy};
use crate::registry;
use serde::Serialize;
use std::io::Read;
use std::process::{Child, Command, ExitCode, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Short network timeouts so a dead connection cannot stall herdr startup.
/// These are git-config hints; a hard wall-clock kill is enforced separately
/// by `run_git_with_timeout`, so a wedged connection cannot hang forever.
/// (`http.connectTimeout` is not a real git-config key, hence absent.)
const GIT_TIMEOUT_ARGS: [&str; 4] = ["-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=15"];

/// Classification of a plugin relative to its upstream ref (v0.2 granularity).
/// `unknown` covers check errors and non-GitHub entries; v0.3 refines
/// `changed` into `behind`/`ahead`/`diverged` via the compare API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Upstream ref matches the installed commit.
    Same,
    /// Upstream ref differs from the installed commit.
    Changed,
    /// Remote could not be resolved (network error, invalid entry, skipped kind).
    Unknown,
}

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
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Install the available update.
    Update,
    /// Keep the installed version (policy, exclude, allow, or no change).
    Hold,
    /// Upstream state could not be determined.
    Error,
}

/// One plugin's verdict from the Policy Engine: what `apply` would do with
/// it, and why. `plan` prints these; `apply` executes `Update` entries.
#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub plugin_id: String,
    pub owner: String,
    pub repo: String,
    pub installed_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_sha: Option<String>,
    pub status: Status,
    pub policy: crate::config::Policy,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    // `update` is the composition of plan + apply (§5.7): analyze with the
    // Policy Engine, then execute the UPDATE actions.
    run_apply(cfg, json, only)
}

/// `plan`: analyze only — print what would happen, execute nothing. Exit
/// code follows the unified contract: `2` on check errors, `1` when updates
/// are available, `0` otherwise.
pub fn run_plan(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let statuses = match collect(cfg, only) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let plan = build_plan(cfg, &statuses);
    if json {
        println!("{}", serde_json::to_string_pretty(&plan).unwrap());
    } else {
        for e in &plan {
            println!("{}", e.plugin_id);
            println!("  installed: {}", short(&e.installed_sha));
            if let Some(r) = &e.remote_sha {
                println!("  remote: {}", short(r));
            }
            println!("  status: {}", status_label(e.status));
            println!("  policy: {}", policy_label(e.policy));
            println!("  action: {}", action_label(e.action));
            if let Some(r) = &e.reason {
                println!("  reason: {r}");
            }
            println!();
        }
    }
    let pending = statuses.iter().filter(|s| s.update_available).count();
    let errors = statuses.iter().filter(|s| s.error.is_some()).count();
    if errors > 0 {
        ExitCode::from(2)
    } else if pending > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `apply`: execute the plan — install every plugin whose action is UPDATE.
/// Held and errored entries are reported but not installed.
pub fn run_apply(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let statuses = match collect(cfg, only) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let plan = build_plan(cfg, &statuses);

    let mut updated: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut held: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for e in &plan {
        match e.action {
            Action::Error => {
                if !json {
                    if let Some(err) = &e.reason {
                        eprintln!("  [{}] error: {err}", e.plugin_id);
                    }
                }
                errors.push(e.plugin_id.clone());
            }
            Action::Hold => {
                if e.status == Status::Same {
                    if !json {
                        println!(
                            "  [{}] up to date ({})",
                            e.plugin_id,
                            short(&e.installed_sha)
                        );
                    }
                    continue;
                }
                if cfg.is_excluded(&e.plugin_id) {
                    excluded.push(e.plugin_id.clone());
                    if !json {
                        println!(
                            "  [{}] update available ({}) but excluded",
                            e.plugin_id,
                            short(&e.installed_sha)
                        );
                    }
                } else {
                    held.push(e.plugin_id.clone());
                    if !json {
                        let why = e.reason.as_deref().unwrap_or("held");
                        println!(
                            "  [{}] update available ({}) but held ({why})",
                            e.plugin_id,
                            short(&e.installed_sha)
                        );
                    }
                }
            }
            Action::Update => {
                if !json {
                    let pin = e
                        .requested_ref
                        .as_deref()
                        .map(|r| format!(" (pinned {r})"))
                        .unwrap_or_default();
                    println!(
                        "  [{}] updating {} -> {}{}",
                        e.plugin_id,
                        short(&e.installed_sha),
                        short(e.remote_sha.as_deref().unwrap_or("?")),
                        pin
                    );
                }
                if apply_update(&e.owner, &e.repo, e.requested_ref.as_deref()) {
                    updated.push(e.plugin_id.clone());
                } else {
                    failed.push(e.plugin_id.clone());
                }
            }
        }
    }

    if json {
        let report = serde_json::json!({
            "updated": updated,
            "failed": failed,
            "excluded": excluded,
            "held": held,
            "errors": errors,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        let mut summary = format!(
            "[herdr-auto-update] {} updated, {} failed",
            updated.len(),
            failed.len()
        );
        if !held.is_empty() {
            summary.push_str(&format!(", {} held", held.len()));
        }
        if !errors.is_empty() {
            summary.push_str(&format!(", {} error(s)", errors.len()));
        }
        eprintln!("{summary}");
        notify(cfg, updated.len(), failed.len(), errors.len(), held.len());
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

/// Policy Engine: decide the action for every plugin. Pure — no I/O — so
/// `plan` and `apply` share one decision path.
fn build_plan(cfg: &Config, statuses: &[PluginStatus]) -> Vec<PlanEntry> {
    statuses
        .iter()
        .map(|s| {
            let (action, reason) = decide(cfg, s);
            PlanEntry {
                plugin_id: s.plugin_id.clone(),
                owner: s.owner.clone(),
                repo: s.repo.clone(),
                installed_sha: s.installed_sha.clone(),
                remote_sha: s.remote_sha.clone(),
                status: s.status,
                policy: cfg.policy,
                action,
                requested_ref: s.requested_ref.clone(),
                reason,
            }
        })
        .collect()
}

fn decide(cfg: &Config, s: &PluginStatus) -> (Action, Option<String>) {
    if let Some(err) = &s.error {
        return (Action::Error, Some(err.clone()));
    }
    if !s.update_available {
        return (Action::Hold, Some("up to date".to_string()));
    }
    if cfg.is_excluded(&s.plugin_id) {
        return (Action::Hold, Some("excluded".to_string()));
    }
    if !cfg.is_allowed(&s.owner, &s.repo) {
        return (Action::Hold, Some("not in allow list".to_string()));
    }
    match cfg.policy {
        Policy::Notify => (Action::Hold, Some("policy: notify".to_string())),
        Policy::PinnedOnly if s.requested_ref.is_none() => {
            (Action::Hold, Some("policy: pinned-only".to_string()))
        }
        Policy::Auto | Policy::PinnedOnly => (Action::Update, None),
    }
}

/// Best-effort desktop notification through herdr's CLI. Failures are
/// ignored: a missing notification API must not change the exit code.
fn notify(cfg: &Config, updated: usize, failed: usize, errors: usize, held: usize) {
    if !cfg.notify || (updated == 0 && failed == 0 && errors == 0 && held == 0) {
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if updated > 0 {
        parts.push(format!("{updated} plugin(s) updated"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if errors > 0 {
        parts.push(format!("{errors} could not be checked"));
    }
    if held > 0 {
        parts.push(format!("{held} held by policy"));
    }
    let body = if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(", ")
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
                status: Status::Unknown,
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
                status: Status::Unknown,
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
                    status: if sha == job.installed_sha {
                        Status::Same
                    } else {
                        Status::Changed
                    },
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
                    status: Status::Unknown,
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
                    status: Status::Unknown,
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
                    kill_process_tree(&mut child);
                    return Err(format!("git ls-remote timed out after {timeout_secs}s"));
                }
            }
            Err(e) => {
                kill_process_tree(&mut child);
                return Err(format!("cannot wait for git: {e}"));
            }
        }
        // Brief sleep so a hung child does not busy-spin the worker.
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Terminate the child and its whole process tree, then reap it.
///
/// On Unix, killing the direct child is enough. On Windows a `.cmd`/`.bat`
/// stub runs under `cmd.exe /c`, and `Child::kill()` (TerminateProcess)
/// only terminates that direct child — grandchildren such as `ping` or
/// git's ssh/remote-helper children survive and `wait()` blocks until they
/// exit on their own. `taskkill /T /F` kills the tree, so the deadline is
/// actually enforced. Windows-only: on Unix this is just kill + wait.
fn kill_process_tree(child: &mut Child) {
    // Order matters on Windows: `taskkill /T /F` must run while the tree is
    // still intact. `Child::kill()` (TerminateProcess) terminates only the
    // direct child — for a `.cmd`/`.bat` stub that is `cmd.exe` itself —
    // orphaning grandchildren (ping, ssh, git remote-helpers) that keep the
    // inherited pipe write-ends open. A later taskkill then fails (exit 128)
    // because the dead cmd.exe is no longer enumerable, and `wait()` blocks
    // until the orphans exit on their own.
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
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

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Same => "same",
        Status::Changed => "changed",
        Status::Unknown => "unknown",
    }
}

fn policy_label(p: Policy) -> &'static str {
    match p {
        Policy::Auto => "auto",
        Policy::Notify => "notify",
        Policy::PinnedOnly => "pinned-only",
    }
}

fn action_label(a: Action) -> &'static str {
    match a {
        Action::Update => "UPDATE",
        Action::Hold => "HOLD",
        Action::Error => "ERROR",
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
