// Core logic: compare each installed github plugin against its upstream HEAD
// via `git ls-remote`, then reinstall outdated ones through the herdr CLI.
// All subprocess calls use argv arrays - no shell interpolation anywhere.

use crate::config::Config;
use crate::registry;
use serde::Serialize;
use std::process::{Command, ExitCode};

/// Short network timeouts so a dead connection cannot stall herdr startup.
const GIT_TIMEOUT_ARGS: [&str; 6] = [
    "-c",
    "http.connectTimeout=10",
    "-c",
    "http.lowSpeedLimit=1",
    "-c",
    "http.lowSpeedTime=15",
];

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub plugin_id: String,
    pub owner: String,
    pub repo: String,
    pub installed_sha: String,
    pub remote_sha: Option<String>,
    pub update_available: bool,
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
    run_update(cfg, json)
}

pub fn run_check(json: bool) -> ExitCode {
    let statuses = match collect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let pending = statuses.iter().filter(|s| s.update_available).count();
    if json {
        println!("{}", serde_json::to_string_pretty(&statuses).unwrap());
    } else {
        for s in &statuses {
            print_status(s);
        }
        eprintln!(
            "[herdr-auto-update] {} plugin(s) checked, {} update(s) available",
            statuses.len(),
            pending
        );
    }
    if pending > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub fn run_update(cfg: &Config, json: bool) -> ExitCode {
    let statuses = match collect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let mut updated: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();

    for s in &statuses {
        if let Some(err) = &s.error {
            if !json {
                eprintln!("  [{}] error: {err}", s.plugin_id);
            }
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
            println!(
                "  [{}] updating {} -> {}",
                s.plugin_id,
                short(&s.installed_sha),
                short(s.remote_sha.as_deref().unwrap_or("?"))
            );
        }
        if apply_update(&s.owner, &s.repo) {
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
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        eprintln!(
            "[herdr-auto-update] {} updated, {} failed",
            updated.len(),
            failed.len()
        );
    }

    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Reinstall one plugin through herdr's own installer (herdr v1 has no
/// dedicated `plugin update`; reinstall replaces the managed checkout while
/// preserving plugin config/state). Flag order matters: `--yes` must come
/// after the positional.
fn apply_update(owner: &str, repo: &str) -> bool {
    let bin = registry::herdr_bin();
    let spec = format!("{owner}/{repo}");
    match Command::new(&bin)
        .args(["plugin", "install", &spec, "--yes"])
        .status()
    {
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

fn collect() -> Result<Vec<PluginStatus>, String> {
    let plugins = registry::list_installed()?;
    let mut out = Vec::new();
    for p in plugins {
        let Some(src) = &p.source else { continue };
        if src.kind != registry::GITHUB_KIND {
            continue; // local links and other kinds are not updatable
        }
        // A github entry may be missing owner/repo/resolved_commit (e.g. a
        // stale registry row); treat it as invalid instead of panicking.
        let (Some(owner), Some(repo), Some(rc)) = (&src.owner, &src.repo, &src.resolved_commit)
        else {
            out.push(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: src.owner.clone().unwrap_or_default(),
                repo: src.repo.clone().unwrap_or_default(),
                installed_sha: src.resolved_commit.clone().unwrap_or_default(),
                remote_sha: None,
                update_available: false,
                error: Some("github source missing owner/repo/commit fields".to_string()),
            });
            continue;
        };
        if !registry::valid_github_name(owner, 39) || !registry::valid_github_name(repo, 100) {
            out.push(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                installed_sha: rc.clone(),
                remote_sha: None,
                update_available: false,
                error: Some("invalid owner/repo recorded in registry".to_string()),
            });
            continue;
        }
        match remote_head(owner, repo) {
            Ok(Some(sha)) => out.push(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                installed_sha: rc.clone(),
                remote_sha: Some(sha.clone()),
                update_available: sha != *rc,
                error: None,
            }),
            Ok(None) => out.push(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                installed_sha: rc.clone(),
                remote_sha: None,
                update_available: false,
                error: Some("cannot resolve remote HEAD (repo moved or deleted?)".to_string()),
            }),
            Err(e) => out.push(PluginStatus {
                plugin_id: p.plugin_id.clone(),
                owner: owner.clone(),
                repo: repo.clone(),
                installed_sha: rc.clone(),
                remote_sha: None,
                update_available: false,
                error: Some(e),
            }),
        }
    }
    Ok(out)
}

fn remote_head(owner: &str, repo: &str) -> Result<Option<String>, String> {
    let url = format!("https://github.com/{owner}/{repo}");
    let out = Command::new(git_bin())
        .args(GIT_TIMEOUT_ARGS)
        .args(["ls-remote", &url, "HEAD"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
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
