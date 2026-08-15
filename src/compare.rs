// Upstream commit-relation classification via the GitHub compare API
// (`GET /repos/{owner}/{repo}/compare/{base}...{head}`), cached to respect
// the unauthenticated rate limit (60 req/h). curl runs as a plain subprocess
// with argv arrays - no shell interpolation.
//
// The compare API reports the relation of `head` to `base`. We call it with
// base = installed commit, head = remote commit, so the API's `ahead` means
// "remote has commits the installed version lacks" -> plugin is behind
// upstream (fast-forwardable). The caller maps API status onto the plugin's
// own `Status`:
//
//   identical -> Same     ahead  -> Behind   behind -> Ahead   diverged -> Diverged

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Output;

/// GitHub compare API status (relation of head to base).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompareStatus {
    Identical,
    /// head (remote) has commits base (installed) lacks -> fast-forwardable.
    Ahead,
    /// base (installed) has commits head (remote) lacks -> local is ahead.
    Behind,
    /// Both sides have unique commits (force push etc.).
    Diverged,
}

/// One cached verdict for an exact (owner, repo, installed, remote) tuple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub owner: String,
    pub repo: String,
    pub installed_sha: String,
    pub remote_sha: String,
    pub status: CompareStatus,
    pub at: String,
}

/// On-disk compare cache (`compare-cache.json` in the plugin config dir).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub entries: Vec<CacheEntry>,
}

const CACHE_VERSION: u32 = 1;

/// curl binary; HERDR_AUTO_UPDATE_CURL overrides it (used by the test suite
/// to stub network access).
pub fn curl_bin() -> String {
    std::env::var("HERDR_AUTO_UPDATE_CURL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "curl".to_string())
}

/// Load the cache file; any read/parse failure yields an empty cache (a
/// corrupt cache must not break updates, it is advisory only).
pub fn load_cache(dir: &Path) -> Cache {
    let text = match std::fs::read_to_string(cache_path(dir)) {
        Ok(t) => t,
        Err(_) => return Cache::default(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Merge fresh verdicts into the cache file and persist it. `new_entries`
/// win over older entries for the same tuple. Failures are ignored: the
/// cache is advisory and must never change an exit code.
pub fn merge_cache(dir: &Path, new_entries: Vec<CacheEntry>) {
    let mut cache = load_cache(dir);
    for ne in new_entries {
        cache.entries.retain(|e| {
            !(e.owner == ne.owner
                && e.repo == ne.repo
                && e.installed_sha == ne.installed_sha
                && e.remote_sha == ne.remote_sha)
        });
        cache.entries.push(ne);
    }
    cache.version = CACHE_VERSION;
    let Ok(text) = serde_json::to_string_pretty(&cache) else {
        return;
    };
    let _ = std::fs::write(cache_path(dir), text);
}

/// Classify `installed` vs `remote` for a repo. Cache-first: an exact tuple
/// hit skips the API. `Err` on network/HTTP failure (rate limit included) -
/// callers degrade to `Status::Unknown` (safe side).
pub fn classify(
    owner: &str,
    repo: &str,
    installed_sha: &str,
    remote_sha: &str,
    timeout_secs: u64,
    cache: &Cache,
) -> Result<CompareStatus, String> {
    if let Some(e) = cache.entries.iter().find(|e| {
        e.owner == owner
            && e.repo == repo
            && e.installed_sha == installed_sha
            && e.remote_sha == remote_sha
    }) {
        return Ok(e.status);
    }

    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/compare/{installed_sha}...{remote_sha}"
    );
    let t = timeout_secs.to_string();
    let args: Vec<&str> = vec!["-fsS", "--max-time", &t, &url];
    let out = crate::updater::run_with_timeout(&curl_bin(), &args, timeout_secs)
        .map_err(|e| format!("compare API: {e}"))?;
    if !out.status.success() {
        return Err(compare_error(&out, timeout_secs));
    }
    let parsed: ApiCompare = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("compare API: cannot parse response: {e}"))?;
    match parsed.status.as_str() {
        "identical" => Ok(CompareStatus::Identical),
        "ahead" => Ok(CompareStatus::Ahead),
        "behind" => Ok(CompareStatus::Behind),
        "diverged" => Ok(CompareStatus::Diverged),
        other => Err(format!("compare API: unexpected status '{other}'")),
    }
}

#[derive(Deserialize)]
struct ApiCompare {
    status: String,
}

fn compare_error(out: &Output, timeout_secs: u64) -> String {
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if err.contains("403") || err.contains("429") || err.contains("rate limit") {
        return "compare API: rate limit exceeded (cache hit next run)".to_string();
    }
    if err.contains("timed out after {timeout_secs}s") || err.contains("timed out after") {
        return format!("compare API: request timed out after {timeout_secs}s");
    }
    let msg = err.trim();
    if msg.is_empty() {
        format!("compare API: request failed (HTTP {})", out.status)
    } else {
        format!("compare API: request failed: {msg}")
    }
}

/// Full path of the compare-cache file.
pub fn cache_path(dir: &Path) -> PathBuf {
    dir.join("compare-cache.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip_and_dedupe() {
        let dir = std::env::temp_dir().join(format!("hau-cmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = CacheEntry {
            owner: "ragamo".into(),
            repo: "herdr-flock".into(),
            installed_sha: "a".repeat(40),
            remote_sha: "b".repeat(40),
            status: CompareStatus::Ahead,
            at: "2026-08-15T00:00:00Z".into(),
        };
        let e2 = CacheEntry {
            status: CompareStatus::Diverged,
            ..e.clone()
        };
        merge_cache(&dir, vec![e.clone()]);
        merge_cache(&dir, vec![e2.clone()]);
        let cache = load_cache(&dir);
        assert_eq!(cache.entries.len(), 1, "same tuple must dedupe");
        assert_eq!(cache.entries[0].status, CompareStatus::Diverged);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_cache_is_empty() {
        let dir = std::env::temp_dir().join(format!("hau-cmp-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("compare-cache.json"), "not json{{").unwrap();
        assert_eq!(load_cache(&dir).entries.len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_hits_cache_without_network() {
        let e = CacheEntry {
            owner: "ragamo".into(),
            repo: "herdr-flock".into(),
            installed_sha: "a".repeat(40),
            remote_sha: "b".repeat(40),
            status: CompareStatus::Behind,
            at: "x".into(),
        };
        let cache = Cache {
            version: 1,
            entries: vec![e],
        };
        let got = classify(
            "ragamo",
            "herdr-flock",
            &"a".repeat(40),
            &"b".repeat(40),
            5,
            &cache,
        )
        .unwrap();
        assert_eq!(got, CompareStatus::Behind);
    }
}
