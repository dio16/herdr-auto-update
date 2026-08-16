// Core logic: compare each installed github plugin against its upstream HEAD
// via `git ls-remote`, then reinstall outdated ones through the herdr CLI.
// All subprocess calls use argv arrays - no shell interpolation anywhere.

use crate::compare;
use crate::config::{Config, Policy};
use crate::registry;
use crate::state;
use serde::Serialize;
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
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
/// Classification of a plugin's installed commit vs upstream (compare API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Upstream ref matches the installed commit.
    Same,
    /// Installed is behind upstream (remote has newer commits) - updateable.
    Behind,
    /// Local is ahead of upstream (upstream was reset/force-pushed away).
    Ahead,
    /// Both sides have unique commits (force push etc.) - not a fast-forward.
    Diverged,
    /// Remote could not be resolved (network error, invalid entry, skipped kind).
    Unknown,
}

/// What kind of ref a plugin is pinned to (`--ref` at install time).
/// Unpinned plugins track the default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// Tracks a branch (explicit `--ref` or unpinned = default branch).
    Branch,
    /// Pinned to a tag (mutable-ish: a tag can be force-moved upstream).
    Tag,
    /// Pinned to an exact commit SHA - immutable by construction.
    Commit,
}

/// Classify a `requested_ref` into a channel kind. A 40-hex ref is a commit;
/// `refs/tags/*` is a tag; everything else (incl. `refs/heads/*` and `None`)
/// is a branch.
pub fn ref_kind(requested_ref: Option<&str>) -> RefKind {
    match requested_ref {
        Some(r) if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) => RefKind::Commit,
        Some(r) if r.starts_with("refs/tags/") => RefKind::Tag,
        _ => RefKind::Branch,
    }
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
    /// Upstream release name (newest tag / pinned tag) for a changed
    /// plugin; display metadata only, never used for decisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_version: Option<String>,
    pub update_available: bool,
    pub status: Status,
    pub ref_kind: RefKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Informational note explaining a status beyond the machine fields
    /// (e.g. "pinned by rollback; run `resume`"). Never changes the action
    /// on its own; `plan`/`check` surface it as the hold reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_version: Option<String>,
    pub status: Status,
    pub ref_kind: RefKind,
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
    let statuses = match collect(cfg, only, json) {
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
    let statuses = match collect(cfg, only, json) {
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
            println!("  installed: {}", version_str(&e.version, &e.installed_sha));
            if let Some(r) = &e.remote_sha {
                println!("  remote: {}", version_str(&e.remote_version, r));
            }
            println!("  status: {}", status_label(e.status));
            println!("  channel: {}", ref_kind_label(e.ref_kind));
            println!("  policy: {}", policy_label(e.policy));
            println!("  action: {}", action_label(e.action));
            if let Some(r) = &e.reason {
                println!("  reason: {r}");
            }
            println!();
        }
    }
    // Contract: exit 1 means "updates would apply" (at least one UPDATE
    // action under the active policy), not merely "some upstream ref
    // changed". A notify-policy plan that holds everything exits 0 even
    // though upstream moved (v1.0.1 P0 fix).
    let pending = plan.iter().any(|p| p.action == Action::Update);
    let errors = statuses.iter().filter(|s| s.error.is_some()).count();
    if errors > 0 {
        ExitCode::from(2)
    } else if pending {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `apply`: execute the plan — install every plugin whose action is UPDATE.
/// Held and errored entries are reported but not installed.
pub fn run_apply(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let statuses = match collect(cfg, only, json) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let plan = build_plan(cfg, &statuses);

    if !json && cfg.policy == Policy::Notify && !cfg.policy_explicit {
        eprintln!(
            "[herdr-auto-update] note: `policy` is not set; defaulting to \"notify\" (v1.0). \
             Set policy = \"auto\" to reinstall updates automatically."
        );
    }

    let mut updated: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut held: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    // Commit-pinned plugins: immutable by construction, never auto-updated
    // (v0.4 channels). Reported separately so startup/update runs surface
    // why a plugin can never update (v1.0.4: pin notice).
    let mut pinned: Vec<String> = Vec::new();

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
                let from = version_str(&e.version, &e.installed_sha);
                let to = version_str(&e.remote_version, e.remote_sha.as_deref().unwrap_or("?"));
                if e.status == Status::Same {
                    if e.ref_kind == RefKind::Commit {
                        let note = e.reason.as_deref().unwrap_or("");
                        // Manual `update` from a terminal: offer to reinstall
                        // the pinned plugin from its default branch, which
                        // clears the pin and moves it to the latest commit in
                        // one step. herdr startup and scripts (non-TTY stdin)
                        // never prompt - they keep the pinned report only.
                        // Rollback quarantines stay on `resume` (it restores
                        // the original tracking ref, not just the default
                        // branch).
                        let interactive = !json && std::io::stdin().is_terminal();
                        if interactive && !note.starts_with("pinned by rollback") {
                            print!(
                                "  [{}] pinned to a commit (installed {from}); reinstall \
                                 from the default branch to update? [y/N] ",
                                e.plugin_id
                            );
                            let _ = std::io::stdout().flush();
                            let mut ans = String::new();
                            let _ = std::io::stdin().read_line(&mut ans);
                            if yes_answer(&ans) {
                                if apply_update(&e.owner, &e.repo, None) {
                                    let current_sha = resolved_commit_of(&e.plugin_id)
                                        .unwrap_or_else(|| e.installed_sha.clone());
                                    record_pin_update(cfg, e, &current_sha);
                                    if !json {
                                        println!(
                                            "  [{}] updated: reinstalled from the default \
                                             branch (installed {from} -> latest {})",
                                            e.plugin_id,
                                            short(&current_sha)
                                        );
                                    }
                                    updated.push(e.plugin_id.clone());
                                } else {
                                    if !json {
                                        eprintln!("  [{}] reinstall failed", e.plugin_id);
                                    }
                                    failed.push(e.plugin_id.clone());
                                }
                                continue;
                            }
                            if !json {
                                println!("  [{}] keeping commit pin", e.plugin_id);
                            }
                        }
                        pinned.push(e.plugin_id.clone());
                        if !json {
                            let hint = if note.starts_with("pinned by rollback") {
                                " (pinned by rollback; run `resume`)"
                            } else {
                                " (commit-pinned; not auto-updated; run `update` from a \
                                  terminal to update it interactively)"
                            };
                            println!(
                                "  [{}] up to date (installed {from}, latest {from}){hint}",
                                e.plugin_id
                            );
                        }
                        continue;
                    }
                    if !json {
                        println!(
                            "  [{}] up to date (installed {from}, latest {from})",
                            e.plugin_id
                        );
                    }
                    continue;
                }
                if cfg.is_excluded(&e.plugin_id) {
                    excluded.push(e.plugin_id.clone());
                    if !json {
                        println!(
                            "  [{}] update available (installed {from} -> latest {to}) but \
                             excluded",
                            e.plugin_id
                        );
                    }
                } else {
                    held.push(e.plugin_id.clone());
                    if !json {
                        let why = e.reason.as_deref().unwrap_or("held");
                        println!(
                            "  [{}] update available (installed {from} -> latest {to}) but \
                             held ({why})",
                            e.plugin_id
                        );
                    }
                }
            }
            Action::Update => {
                let from = version_str(&e.version, &e.installed_sha);
                let to = version_str(&e.remote_version, e.remote_sha.as_deref().unwrap_or("?"));
                let pin = e
                    .requested_ref
                    .as_deref()
                    .map(|r| format!(" (pinned {r})"))
                    .unwrap_or_default();
                let animated = !json && std::io::stdout().is_terminal();
                if !json && !animated {
                    println!(
                        "  [{}] updating installed {from} -> latest {to}{pin}",
                        e.plugin_id
                    );
                }
                let label = format!(
                    "updating {} installed {from} -> latest {to}{pin}",
                    e.plugin_id
                );
                let ok = crate::progress::with_activity(&label, {
                    let owner = e.owner.clone();
                    let repo = e.repo.clone();
                    let requested_ref = e.requested_ref.clone();
                    move || apply_update(&owner, &repo, requested_ref.as_deref())
                });
                if animated {
                    if ok {
                        println!(
                            "  [{}] updated installed {from} -> latest {to}{pin}",
                            e.plugin_id
                        );
                    } else {
                        eprintln!("  [{}] update failed ({to})", e.plugin_id);
                    }
                }
                if ok {
                    updated.push(e.plugin_id.clone());
                    record_update(cfg, e);
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
            "pinned": pinned,
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
        if !pinned.is_empty() {
            summary.push_str(&format!(", {} pinned", pinned.len()));
            eprintln!(
                "[herdr-auto-update] {} plugin(s) are pinned to commits and cannot be \
                 auto-updated: {}",
                pinned.len(),
                pinned.join(", ")
            );
            eprintln!(
                "[herdr-auto-update] hint: run `herdr-auto-update update` from a terminal to \
                 update pinned plugins interactively; `untrack --only <plugin_id>` switches \
                 one to the default branch non-interactively"
            );
        }
        if !errors.is_empty() {
            summary.push_str(&format!(", {} error(s)", errors.len()));
        }
        eprintln!("{summary}");
        notify(
            cfg,
            updated.len(),
            failed.len(),
            errors.len(),
            held.len(),
            pinned.len(),
        );
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
                version: s.version.clone(),
                remote_sha: s.remote_sha.clone(),
                remote_version: s.remote_version.clone(),
                status: s.status,
                ref_kind: s.ref_kind,
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
    // Only fast-forwardable updates (Behind) are ever applied. Same / ahead /
    // diverged / unknown never update - installing them could lose commits.
    let why = match s.status {
        Status::Same => Some(s.note.clone().unwrap_or_else(|| "up to date".to_string())),
        Status::Ahead => {
            Some("installed commit is ahead of upstream (upstream reset?)".to_string())
        }
        Status::Unknown => Some("cannot classify update (network / rate limit?)".to_string()),
        Status::Behind => None,
        Status::Diverged => {
            if cfg.allow_force_push {
                None
            } else {
                Some("commits diverged (force push?); allow_force_push=false".to_string())
            }
        }
    };
    if let Some(why) = why {
        return (Action::Hold, Some(why));
    }
    if cfg.is_excluded(&s.plugin_id) {
        return (Action::Hold, Some("excluded".to_string()));
    }
    if !cfg.is_allowed(&s.owner, &s.repo) {
        return (Action::Hold, Some("not in allow list".to_string()));
    }
    if !cfg.trusted_owners.is_empty() && !cfg.trusted_owners.iter().any(|o| o == &s.owner) {
        return (
            Action::Hold,
            Some(format!("owner '{}' not trusted", s.owner)),
        );
    }
    if cfg.immutable_pins && s.ref_kind != RefKind::Branch {
        return (
            Action::Hold,
            Some(format!(
                "immutable pin ({}); not auto-updated",
                ref_kind_label(s.ref_kind)
            )),
        );
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
/// `pinned` covers commit-pinned plugins (never auto-updateable) so a
/// startup run surfaces them instead of silently reporting "up to date".
fn notify(cfg: &Config, updated: usize, failed: usize, errors: usize, held: usize, pinned: usize) {
    if !cfg.notify || (updated == 0 && failed == 0 && errors == 0 && held == 0 && pinned == 0) {
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
    if pinned > 0 {
        parts.push(format!("{pinned} pinned to commits (not auto-updated)"));
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

/// Record a successful update in state.json (§5.8), then verify herdr's
/// registry now resolves the plugin to the new commit. Best-effort: a write
/// failure or verify mismatch is reported, never fatal.
fn record_update(cfg: &Config, e: &PlanEntry) {
    let Some(dir) = cfg.data_dir() else { return };
    let mut st = state::State::load(&dir);
    st.append(state::StateEntry {
        plugin_id: e.plugin_id.clone(),
        previous_sha: e.installed_sha.clone(),
        current_sha: e.remote_sha.clone().unwrap_or_default(),
        requested_ref: e.requested_ref.clone(),
        updated_at: state::rfc3339_now(),
        result: "updated".to_string(),
    });
    if let Err(err) = st.save(&dir) {
        eprintln!("  warning: {err}");
        return;
    }
    if let Some(expected) = &e.remote_sha {
        match verify_installed(&e.plugin_id, expected) {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "  [{}] warning: verify failed - registry still reports the old commit",
                e.plugin_id
            ),
            Err(err) => eprintln!("  [{}] warning: verify error: {err}", e.plugin_id),
        }
    }
}

/// Record a pin-clearing reinstall as an update entry (v1.0.8): the plugin
/// moved from a commit pin to the default branch's latest commit via the
/// interactive `update` path.
fn record_pin_update(cfg: &Config, e: &PlanEntry, new_sha: &str) {
    let Some(dir) = cfg.data_dir() else { return };
    let mut st = state::State::load(&dir);
    st.append(state::StateEntry {
        plugin_id: e.plugin_id.clone(),
        previous_sha: e.installed_sha.clone(),
        current_sha: new_sha.to_string(),
        requested_ref: None,
        updated_at: state::rfc3339_now(),
        result: "updated".to_string(),
    });
    if let Err(err) = st.save(&dir) {
        eprintln!("  warning: {err}");
    }
}

/// Interactive prompt answer: `y` / `yes` (case-insensitive) accept.
fn yes_answer(ans: &str) -> bool {
    matches!(ans.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Confirm herdr's registry resolves `plugin_id` to `expected` (the commit we
/// just installed). Uses the same registry path as `check`, so a stale
/// registry row reads as not-verified.
fn verify_installed(plugin_id: &str, expected: &str) -> Result<bool, String> {
    let plugins = registry::list_installed()?;
    Ok(plugins
        .iter()
        .find(|p| p.plugin_id == plugin_id)
        .and_then(|p| p.source.as_ref())
        .and_then(|s| s.resolved_commit.as_deref())
        .map(|sha| sha == expected)
        .unwrap_or(false))
}

/// `rollback`: reinstall a plugin from the commit recorded before its last
/// update. Relies on herdr accepting a commit SHA via `--ref` (§8 risk).
pub fn run_rollback(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let Some(dir) = cfg.data_dir() else {
        eprintln!("error: no config dir; cannot locate state.json");
        return ExitCode::from(2);
    };
    let mut st = state::State::load(&dir);

    // Roll back only the most recent update per plugin. Older history is
    // left alone: rolling back an A->B->C trail must install B once, not
    // rewind through every recorded update (v1.0.1 P0 fix). A plugin whose
    // newest entry is already `rolled_back` is skipped - it is already
    // rolled back, and resume is the way back onto the tracking ref.
    // The empty-state early return is scoped to the no-`--only` path so an
    // unknown id can never silently no-op (v1.0.4: `--only` contract).
    let targets: Vec<state::StateEntry> = match only {
        Some(id) => match st.latest_for(id) {
            Some(e) if e.result == "updated" => vec![e.clone()],
            Some(_) => {
                eprintln!("  [{id}] already rolled back; run `resume` to rejoin the tracking ref");
                Vec::new()
            }
            None => {
                eprintln!("error: no update recorded for plugin '{id}'");
                return ExitCode::from(2);
            }
        },
        None => {
            if st.entries.is_empty() {
                if !json {
                    eprintln!("[herdr-auto-update] no updates recorded - nothing to roll back");
                }
                return ExitCode::SUCCESS;
            }
            let mut seen: HashSet<&str> = HashSet::new();
            st.entries
                .iter()
                .rev()
                .filter(|e| seen.insert(e.plugin_id.as_str()))
                .filter(|e| e.result == "updated")
                .cloned()
                .collect()
        }
    };

    let mut rolled_back: Vec<&str> = Vec::new();
    for e in &targets {
        if e.previous_sha.is_empty() {
            eprintln!(
                "  [{}] cannot roll back: no previous commit recorded",
                e.plugin_id
            );
            continue;
        }
        let Some((owner, repo)) = owner_repo_of(&e.plugin_id) else {
            eprintln!(
                "  [{}] cannot roll back: plugin not in registry",
                e.plugin_id
            );
            continue;
        };
        if !json {
            println!(
                "  [{}] rolling back {} -> {}",
                e.plugin_id,
                short(&e.current_sha),
                short(&e.previous_sha)
            );
        }
        if apply_update(&owner, &repo, Some(&e.previous_sha)) {
            // Record the rollback. herdr keeps the rolled-back SHA as
            // requested_ref, so this entry is the only place the original
            // tracking ref survives; `resume` needs it (v1.0.1).
            st.append(state::StateEntry {
                plugin_id: e.plugin_id.clone(),
                previous_sha: e.current_sha.clone(),
                current_sha: e.previous_sha.clone(),
                requested_ref: e.requested_ref.clone(),
                updated_at: state::rfc3339_now(),
                result: "rolled_back".to_string(),
            });
            if let Err(err) = st.save(&dir) {
                eprintln!("  warning: {err}");
            }
            rolled_back.push(&e.plugin_id);
        }
    }
    if !json {
        eprintln!(
            "[herdr-auto-update] {} plugin(s) rolled back",
            rolled_back.len()
        );
    }
    if rolled_back.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `resume`: reinstall a plugin from the tracking ref recorded before its
/// last rollback, undoing the commit pin that rollback introduced and
/// rejoining the auto-update channel (v1.0.1 P0 design fix: rollback =
/// quarantine, resume = back to the original tracking ref).
pub fn run_resume(cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let Some(dir) = cfg.data_dir() else {
        eprintln!("error: no config dir; cannot locate state.json");
        return ExitCode::from(2);
    };
    let mut st = state::State::load(&dir);

    // Most recent `rolled_back` entry per plugin: it carries the original
    // tracking ref (requested_ref) captured before the rollback.
    let targets: Vec<state::StateEntry> = match only {
        Some(id) => match st.latest_for(id) {
            Some(e) if e.result == "rolled_back" => vec![e.clone()],
            _ => {
                eprintln!("error: no rollback recorded for plugin '{id}'");
                return ExitCode::from(2);
            }
        },
        None => {
            if st.entries.is_empty() {
                if !json {
                    eprintln!("[herdr-auto-update] no rollbacks recorded - nothing to resume");
                }
                return ExitCode::SUCCESS;
            }
            let mut seen: HashSet<&str> = HashSet::new();
            st.entries
                .iter()
                .rev()
                .filter(|e| seen.insert(e.plugin_id.as_str()))
                .filter(|e| e.result == "rolled_back")
                .cloned()
                .collect()
        }
    };

    let mut resumed: Vec<&str> = Vec::new();
    for e in &targets {
        let Some((owner, repo)) = owner_repo_of(&e.plugin_id) else {
            eprintln!("  [{}] cannot resume: plugin not in registry", e.plugin_id);
            continue;
        };
        if !json {
            let track = e.requested_ref.as_deref().unwrap_or("<default branch>");
            println!("  [{}] resuming tracking ref {track}", e.plugin_id);
        }
        if apply_update(&owner, &repo, e.requested_ref.as_deref()) {
            // Record the resume as an update entry: the plugin is back on
            // the tracking ref at the current upstream commit.
            let current_sha =
                resolved_commit_of(&e.plugin_id).unwrap_or_else(|| e.current_sha.clone());
            st.append(state::StateEntry {
                plugin_id: e.plugin_id.clone(),
                previous_sha: e.current_sha.clone(),
                current_sha,
                requested_ref: e.requested_ref.clone(),
                updated_at: state::rfc3339_now(),
                result: "updated".to_string(),
            });
            if let Err(err) = st.save(&dir) {
                eprintln!("  warning: {err}");
            }
            resumed.push(&e.plugin_id);
        }
    }
    if !json {
        eprintln!("[herdr-auto-update] {} plugin(s) resumed", resumed.len());
    }
    if resumed.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `untrack`: reinstall a commit-pinned plugin without `--ref`, switching it
/// from a commit pin to default-branch tracking so it becomes auto-updateable
/// again (v1.0.4: the "reinstall or not" choice from the pin notice).
/// Requires `--only <plugin_id>` — reinstalling every plugin ref-less is too
/// broad to do by accident. Does not touch state.json; the pin clears in
/// herdr's registry when the ref-less install resolves.
/// (`cfg` is unused today: untrack drives herdr directly and needs no policy
/// or timeout settings; kept for dispatch symmetry.)
pub fn run_untrack(_cfg: &Config, json: bool, only: Option<&str>) -> ExitCode {
    let Some(id) = only else {
        eprintln!("error: untrack requires --only <plugin_id> (one plugin at a time)");
        eprintln!("usage: herdr-auto-update untrack --only <plugin_id>");
        return ExitCode::from(2);
    };
    let plugins = match registry::list_installed() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(plugin) = plugins.iter().find(|p| p.plugin_id == id) else {
        eprintln!("error: plugin '{id}' not found in the herdr registry");
        return ExitCode::from(2);
    };
    let Some(src) = &plugin.source else {
        eprintln!("error: plugin '{id}' is not a GitHub-installed plugin");
        return ExitCode::from(2);
    };
    let (Some(owner), Some(repo)) = (&src.owner, &src.repo) else {
        eprintln!("error: plugin '{id}' has no owner/repo recorded in the registry");
        return ExitCode::from(2);
    };
    if ref_kind(src.requested_ref.as_deref()) != RefKind::Commit {
        let ref_name = src.requested_ref.as_deref().unwrap_or("<none>");
        eprintln!("error: plugin '{id}' is not pinned to a commit (requested_ref: {ref_name})");
        return ExitCode::from(2);
    }
    if !json {
        println!("  [{id}] untracking: reinstalling {owner}/{repo} without --ref (default branch)");
    }
    if !apply_update(owner, repo, None) {
        return ExitCode::from(1);
    }
    // Confirm herdr actually cleared the pin (ref-less install resolves the
    // default branch and records no requested_ref).
    let still_pinned = registry::list_installed()
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.plugin_id == id))
        .and_then(|p| p.source)
        .map(|s| ref_kind(s.requested_ref.as_deref()) == RefKind::Commit)
        .unwrap_or(false);
    if still_pinned {
        eprintln!("  [{id}] warning: registry still records a commit pin after reinstall",);
        return ExitCode::from(1);
    }
    if !json {
        println!(
            "  [{id}] now tracking the default branch; run `herdr-auto-update update` to \
             apply available updates"
        );
    }
    ExitCode::SUCCESS
}

/// `history`: print the recorded update/rollback trail from state.json.
pub fn run_history(cfg: &Config, json: bool) -> ExitCode {
    let Some(dir) = cfg.data_dir() else {
        eprintln!("error: no config dir; cannot locate state.json");
        return ExitCode::from(2);
    };
    let st = state::State::load(&dir);
    if json {
        println!("{}", serde_json::to_string_pretty(&st.entries).unwrap());
        return ExitCode::SUCCESS;
    }
    if st.entries.is_empty() {
        println!("[herdr-auto-update] no recorded updates");
        return ExitCode::SUCCESS;
    }
    for e in &st.entries {
        println!(
            "{} {} {} {} -> {}",
            e.updated_at,
            e.result,
            e.plugin_id,
            short(&e.previous_sha),
            short(&e.current_sha)
        );
    }
    ExitCode::SUCCESS
}

/// Resolve a plugin id to its owner/repo from the current herdr registry.
fn owner_repo_of(plugin_id: &str) -> Option<(String, String)> {
    let plugins = registry::list_installed().ok()?;
    plugins
        .iter()
        .find(|p| p.plugin_id == plugin_id)
        .and_then(|p| {
            let s = p.source.as_ref()?;
            Some((s.owner.clone()?, s.repo.clone()?))
        })
}

/// Current resolved commit for a plugin id from herdr's registry. `None`
/// when the plugin is absent or the registry has no commit recorded.
fn resolved_commit_of(plugin_id: &str) -> Option<String> {
    let plugins = registry::list_installed().ok()?;
    plugins
        .iter()
        .find(|p| p.plugin_id == plugin_id)
        .and_then(|p| p.source.as_ref())
        .and_then(|s| s.resolved_commit.clone())
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
/// can never silently no-op. `json` disables the live progress panel.
fn collect(cfg: &Config, only: Option<&str>, json: bool) -> Result<Vec<PluginStatus>, String> {
    let plugins = registry::list_installed()?;
    // Loaded once for the commit-pinned branch: a plugin whose newest state
    // entry is `rolled_back` is commit-pinned by a rollback (quarantine) and
    // gets a note telling the user `resume` restores tracking (v1.0.1).
    let rollback_state = cfg.data_dir().map(|d| state::State::load(&d));
    let mut progress = crate::progress::Progress::new(plugins.len(), json);

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
        progress.row(i, &p.plugin_id);
        // A github entry may be missing owner/repo/resolved_commit (e.g. a
        // stale registry row); treat it as invalid instead of panicking.
        let (Some(owner), Some(repo), Some(rc)) = (&src.owner, &src.repo, &src.resolved_commit)
        else {
            let status = PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: src.owner.clone().unwrap_or_default(),
                repo: src.repo.clone().unwrap_or_default(),
                version: p.version.clone(),
                installed_sha: src.resolved_commit.clone().unwrap_or_default(),
                remote_sha: None,
                remote_version: None,
                update_available: false,
                status: Status::Unknown,
                ref_kind: ref_kind(src.requested_ref.as_deref()),
                requested_ref: src.requested_ref.clone(),
                error: Some("github source missing owner/repo/commit fields".to_string()),
                note: None,
            };
            progress.done(i, false, status_line(&status));
            statuses.push(Some(status));
            continue;
        };
        if !registry::valid_github_name(owner, 39) || !registry::valid_github_name(repo, 100) {
            let status = PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                version: p.version.clone(),
                installed_sha: rc.clone(),
                remote_sha: None,
                remote_version: None,
                update_available: false,
                status: Status::Unknown,
                ref_kind: ref_kind(src.requested_ref.as_deref()),
                requested_ref: src.requested_ref.clone(),
                error: Some("invalid owner/repo recorded in registry".to_string()),
                note: None,
            };
            progress.done(i, false, status_line(&status));
            statuses.push(Some(status));
            continue;
        }
        // A commit-pinned plugin is immutable by construction: the pin IS the
        // installed commit, so it can never be behind upstream and is never
        // updated. Skip the network entirely (v0.4 ref channels).
        if ref_kind(src.requested_ref.as_deref()) == RefKind::Commit {
            // Distinguish a rollback quarantine (resume is the way back)
            // from an install-time commit pin (untrack is the way back).
            let note = rollback_state
                .as_ref()
                .and_then(|s| s.latest_for(&p.plugin_id))
                .filter(|e| e.result == "rolled_back")
                .map(|_| "pinned by rollback; run `resume` to rejoin the tracking ref".to_string())
                .or_else(|| {
                    Some(
                        "pinned to a commit; not auto-updated (run `herdr-auto-update update` \
                         from a terminal to update it interactively)"
                            .to_string(),
                    )
                });
            let status = PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                version: p.version.clone(),
                installed_sha: rc.clone(),
                remote_sha: Some(rc.clone()),
                remote_version: None,
                update_available: false,
                status: Status::Same,
                ref_kind: RefKind::Commit,
                requested_ref: src.requested_ref.clone(),
                error: None,
                note,
            };
            progress.done(i, true, status_line(&status));
            statuses.push(Some(status));
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

    // Worker pool: at most `max_concurrency` git/curl processes at once. A
    // registry full of plugins must not spawn an unbounded number of
    // processes, and a slow network must not serialize all checks.
    let workers = jobs.len().min(cfg.max_concurrency.max(1));
    let timeout_secs = cfg.timeout_secs;
    let config_dir = cfg.data_dir();
    let cache = config_dir
        .as_deref()
        .map(compare::load_cache)
        .unwrap_or_default();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let next = &next;
        let jobs = &jobs;
        let cache = &cache;
        let (res_tx, res_rx) = mpsc::channel::<(usize, JobResult)>();
        for _ in 0..workers {
            let res_tx = res_tx.clone();
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= jobs.len() {
                        break;
                    }
                    let job = &jobs[i];
                    let result = resolve_job(job, timeout_secs, cache);
                    // Workers only exit after every job index is claimed, and
                    // the collector only exits after every result arrives.
                    res_tx.send((i, result)).expect("result channel closed");
                }
            });
        }
        drop(res_tx);
        let mut fresh_entries: Vec<compare::CacheEntry> = Vec::new();
        let mut received = 0usize;
        while received < jobs.len() {
            let (index, result) = match res_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(r) => r,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Nothing arrived: animate the in-flight rows. The
                    // channel cannot disconnect before every result arrived,
                    // so a timeout is just a paint tick.
                    progress.tick();
                    continue;
                }
            };
            received += 1;
            let job = &jobs[index];
            fresh_entries.extend(result.cache_entries.clone());
            let status = PluginStatus {
                plugin_id: job.plugin_id.clone(),
                owner: job.owner.clone(),
                repo: job.repo.clone(),
                version: job.version.clone(),
                installed_sha: job.installed_sha.clone(),
                remote_sha: result.remote_sha,
                remote_version: result.remote_version,
                update_available: result.status == Status::Behind,
                status: result.status,
                ref_kind: ref_kind(job.requested_ref.as_deref()),
                requested_ref: job.requested_ref.clone(),
                error: result.error,
                note: None,
            };
            progress.done(job.index, status.error.is_none(), status_line(&status));
            statuses[job.index] = Some(status);
        }
        if let Some(dir) = config_dir.as_deref() {
            compare::merge_cache(dir, fresh_entries);
        }
    });
    progress.finish();

    Ok(statuses.into_iter().flatten().collect())
}

/// Result of resolving one plugin's remote state (ls-remote + compare API).
struct JobResult {
    remote_sha: Option<String>,
    status: Status,
    error: Option<String>,
    remote_version: Option<String>,
    cache_entries: Vec<compare::CacheEntry>,
}

/// Resolve a plugin's remote commit and classify it against the installed
/// commit. API failures degrade to `Unknown` (safe side: no update); the
/// cache keeps rate-limited repos retryable next run.
fn resolve_job(job: &RemoteJob, timeout_secs: u64, cache: &compare::Cache) -> JobResult {
    let remote = match remote_head(
        &job.owner,
        &job.repo,
        job.requested_ref.as_deref(),
        timeout_secs,
    ) {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            return JobResult {
                remote_sha: None,
                status: Status::Unknown,
                error: Some("cannot resolve remote HEAD (repo moved or deleted?)".to_string()),
                remote_version: None,
                cache_entries: Vec::new(),
            }
        }
        Err(e) => {
            return JobResult {
                remote_sha: None,
                status: Status::Unknown,
                error: Some(e),
                remote_version: None,
                cache_entries: Vec::new(),
            }
        }
    };
    if remote == job.installed_sha {
        return JobResult {
            remote_sha: Some(remote),
            status: Status::Same,
            error: None,
            remote_version: None,
            cache_entries: Vec::new(),
        };
    }
    // The remote ref changed: resolve a human version name for the display
    // (newest tag, or the pinned tag itself). Best-effort and only paid for
    // plugins that actually moved.
    let remote_version = remote_version_for(job, timeout_secs);
    // Ref changed: classify via the compare API (cached). Failure degrades
    // to Unknown - we cannot prove it is a fast-forward, so no update.
    match compare::classify(
        &job.owner,
        &job.repo,
        &job.installed_sha,
        &remote,
        timeout_secs,
        cache,
    ) {
        Ok(compare::CompareStatus::Identical) => JobResult {
            remote_sha: Some(remote),
            status: Status::Same,
            error: None,
            remote_version: None,
            cache_entries: Vec::new(),
        },
        Ok(compare::CompareStatus::Ahead) => JobResult {
            remote_sha: Some(remote.clone()),
            status: Status::Behind,
            error: None,
            remote_version,
            cache_entries: cache_entry(job, &remote, compare::CompareStatus::Ahead),
        },
        Ok(compare::CompareStatus::Behind) => JobResult {
            remote_sha: Some(remote.clone()),
            status: Status::Ahead,
            error: None,
            remote_version,
            cache_entries: cache_entry(job, &remote, compare::CompareStatus::Behind),
        },
        Ok(compare::CompareStatus::Diverged) => JobResult {
            remote_sha: Some(remote.clone()),
            status: Status::Diverged,
            error: None,
            remote_version,
            cache_entries: cache_entry(job, &remote, compare::CompareStatus::Diverged),
        },
        Err(e) => JobResult {
            remote_sha: Some(remote),
            status: Status::Unknown,
            error: Some(e),
            remote_version: None,
            cache_entries: Vec::new(),
        },
    }
}

/// Human-readable upstream version for a changed plugin: the pinned tag name
/// when the plugin tracks a tag, else the newest `refs/tags/*` from
/// `git ls-remote --tags --sort=-v:refname` (client-side version sort).
/// Best-effort display metadata; any failure yields `None` (the caller then
/// falls back to the short SHA).
fn remote_version_for(job: &RemoteJob, timeout_secs: u64) -> Option<String> {
    if let Some(r) = &job.requested_ref {
        if let Some(tag) = r.strip_prefix("refs/tags/") {
            return Some(tag.to_string());
        }
    }
    let url = format!("https://github.com/{}/{}", job.owner, job.repo);
    let mut args: Vec<&str> = GIT_TIMEOUT_ARGS.to_vec();
    args.extend(["ls-remote", "--tags", "--sort=-v:refname", &url]);
    let out = run_with_timeout(&git_bin(), &args, timeout_secs).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let _sha = it.next()?;
            let r = it.next()?;
            r.strip_prefix("refs/tags/")
                .map(str::to_string)
                .filter(|t| !t.is_empty() && !t.ends_with("^{}"))
        })
        .next()
}

fn cache_entry(
    job: &RemoteJob,
    remote_sha: &str,
    status: compare::CompareStatus,
) -> Vec<compare::CacheEntry> {
    vec![compare::CacheEntry {
        owner: job.owner.clone(),
        repo: job.repo.clone(),
        installed_sha: job.installed_sha.clone(),
        remote_sha: remote_sha.to_string(),
        status,
        at: state::rfc3339_now(),
    }]
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
    let out = run_with_timeout(&git_bin(), &args, timeout_secs)?;
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

/// Run an external binary with a wall-clock deadline. `timeout_secs == 0`
/// disables the deadline. stdout/stderr are drained on reader threads while
/// the child runs, so a chatty child (e.g. the GitHub compare API returning
/// a multi-hundred-KB JSON body) can never fill the pipe buffer and deadlock
/// against the wait loop; a timed-out process is killed and reaped before we
/// return.
pub fn run_with_timeout(bin: &str, args: &[&str], timeout_secs: u64) -> Result<Output, String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if timeout_secs == 0 {
        return child
            .wait_with_output()
            .map_err(|e| format!("cannot run {bin}: {e}"));
    }
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stdout = stdout;
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_process_tree(&mut child);
                    return Err(format!("{bin} timed out after {timeout_secs}s"));
                }
            }
            Err(e) => {
                kill_process_tree(&mut child);
                return Err(format!("cannot wait for {bin}: {e}"));
            }
        }
        // Brief sleep so a hung child does not busy-spin the worker.
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
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

/// The final report line for a plugin status, e.g.
/// `[flock.farm] up to date (v0.1.0)`. Also used as the live-panel row
/// label, so the panel and the report agree.
fn status_line(s: &PluginStatus) -> String {
    if let Some(err) = &s.error {
        return format!("[{}] error: {err}", s.plugin_id);
    }
    let from = version_str(&s.version, &s.installed_sha);
    let to = version_str(&s.remote_version, s.remote_sha.as_deref().unwrap_or("?"));
    match s.status {
        // "installed X -> latest Y" answers what is on disk now and how far
        // the plugin can go (v1.0.8 UX).
        Status::Behind => format!(
            "[{}] update available: installed {from} -> latest {to}",
            s.plugin_id
        ),
        Status::Ahead => format!(
            "[{}] ahead of upstream (installed {from}, latest {to})",
            s.plugin_id
        ),
        Status::Diverged => format!(
            "[{}] diverged from upstream (installed {from}, latest {to})",
            s.plugin_id
        ),
        Status::Unknown => format!("[{}] unknown (installed {from})", s.plugin_id),
        Status::Same => {
            let mut line = format!(
                "[{}] up to date (installed {from}, latest {from})",
                s.plugin_id
            );
            if let Some(note) = &s.note {
                let hint = if note.starts_with("pinned by rollback") {
                    " (pinned by rollback; run `resume`)"
                } else if note.starts_with("pinned to a commit") {
                    " (commit-pinned; not auto-updated)"
                } else {
                    ""
                };
                line.push_str(hint);
            }
            line
        }
    }
}

fn print_status(s: &PluginStatus) {
    println!("  {}", status_line(s));
}

/// Human version label: the resolved version (manifest / tag) prefixed with
/// `v` for consistency, else the short commit SHA. Display only.
fn version_str(v: &Option<String>, sha: &str) -> String {
    match v.as_deref().filter(|s| !s.is_empty()) {
        Some(v) if v.starts_with('v') => v.to_string(),
        Some(v) => format!("v{v}"),
        None => short(sha).to_string(),
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Same => "same",
        Status::Behind => "behind",
        Status::Ahead => "ahead",
        Status::Diverged => "diverged",
        Status::Unknown => "unknown",
    }
}

fn ref_kind_label(k: RefKind) -> &'static str {
    match k {
        RefKind::Branch => "branch",
        RefKind::Tag => "tag",
        RefKind::Commit => "commit",
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
    fn yes_answer_accepts_y_and_yes_only() {
        assert!(yes_answer("y"));
        assert!(yes_answer("Y"));
        assert!(yes_answer("yes"));
        assert!(yes_answer("  Yes \n"));
        assert!(!yes_answer(""));
        assert!(!yes_answer("n"));
        assert!(!yes_answer("N"));
        assert!(!yes_answer("no"));
        assert!(!yes_answer("maybe"));
    }

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
