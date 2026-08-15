// Plugin configuration, read from HERDR_PLUGIN_CONFIG_DIR/config.toml
// (herdr creates that directory on install) with CLI override support.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Update policy: what `update`/`apply`/`startup` may do with a plugin whose
/// upstream ref changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// Install updates, honoring `allow` / `exclude` (default; v0.2 keeps
    /// v0.1 behavior for compatibility).
    Auto,
    /// Check, report, and notify only; never install.
    Notify,
    /// Only update plugins pinned via `requested_ref` (installed with
    /// `--ref`); unpinned plugins are held.
    PinnedOnly,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Reinstall outdated plugins automatically from the startup hook.
    pub auto_update: bool,
    /// Show a desktop notification via `herdr notification show` when
    /// updates ran (updated and/or failed). Ignored with --json.
    pub notify: bool,
    /// Plugin ids to skip during updates.
    pub exclude: Vec<String>,
    /// Wall-clock deadline (seconds) for one remote check (`git ls-remote`).
    /// Kills the git process so a wedged connection cannot hang forever.
    pub timeout_secs: u64,
    /// Upper bound on concurrent remote checks. Bounded so a registry full of
    /// plugins cannot spawn an unbounded number of git processes.
    pub max_concurrency: usize,
    /// Update policy: auto (default) | notify | pinned-only.
    pub policy: Policy,
    /// Restrict update targets to these `owner/repo` glob patterns
    /// (e.g. `["ragamo/*", "*/herdr-file-viewer"]`). Empty = all GitHub
    /// plugins.
    pub allow: Vec<String>,
    /// Only fast-forward updates are installed. Enforced by classification:
    /// only `behind` (installed commit is an ancestor of the upstream ref)
    /// is ever auto-applied; `ahead` and `diverged` are held.
    pub require_fast_forward: bool,
    /// Allow installing a diverged upstream (force-pushed history). Default
    /// false: diverged is held. Fast-forward protection is unaffected.
    pub allow_force_push: bool,
    /// Path of the loaded config file (set by `load`; absent for defaults).
    /// Parents state.json / compare-cache.json resolution.
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_update: true,
            notify: true,
            exclude: Vec::new(),
            timeout_secs: 20,
            max_concurrency: 8,
            policy: Policy::Auto,
            allow: Vec::new(),
            require_fast_forward: true,
            allow_force_push: false,
            config_path: None,
        }
    }
}

impl Config {
    pub fn is_excluded(&self, plugin_id: &str) -> bool {
        self.exclude.iter().any(|e| e == plugin_id)
    }

    /// Empty `allow` admits every owner/repo; otherwise at least one glob
    /// pattern must match `owner/repo`.
    pub fn is_allowed(&self, owner: &str, repo: &str) -> bool {
        if self.allow.is_empty() {
            return true;
        }
        let spec = format!("{owner}/{repo}");
        self.allow.iter().any(|p| glob_match(p, &spec))
    }
}

/// Minimal glob for `allow` patterns: `*` matches any sequence (including
/// empty, crossing `/`), `?` exactly one non-`/` character. Keeps
/// dependencies at serde/toml only.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti] || (p[pi] == '?' && t[ti] != '/')) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Load config; missing file or unset env vars yield defaults. Parse errors
/// are fatal so a broken config cannot silently change behavior.
pub fn load(override_path: Option<&str>) -> Result<Config, String> {
    let Some(path) = override_path.map(PathBuf::from).or_else(default_path) else {
        return Ok(Config::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("cannot read config {}: {e}", path.display())),
    };
    let mut cfg: Config =
        toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))?;
    cfg.config_path = Some(path);
    Ok(cfg)
}

impl Config {
    /// Directory for the plugin's own data files (state.json, compare
    /// cache): the loaded config file's parent, else the env-derived config
    /// dir. `None` when no config location is known (rollback/history then
    /// report "no state available").
    pub fn data_dir(&self) -> Option<PathBuf> {
        if let Some(p) = &self.config_path {
            return p.parent().map(|d| d.to_path_buf());
        }
        config_dir(None)
    }
}

fn default_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HERDR_PLUGIN_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join("config.toml"));
        }
    }
    if let Ok(root) = std::env::var("HERDR_PLUGIN_ROOT") {
        if !root.trim().is_empty() {
            return Some(PathBuf::from(root).join("config").join("config.toml"));
        }
    }
    None
}

/// Directory for plugin-owned data files (`state.json`, `compare-cache.json`).
/// Mirrors `default_path`: the `--config` file's parent, else
/// `HERDR_PLUGIN_CONFIG_DIR`, else `HERDR_PLUGIN_ROOT/config`. `None` when
/// nothing is resolvable — callers then skip file writes (stateless mode).
pub fn config_dir(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        return PathBuf::from(p).parent().map(|d| d.to_path_buf());
    }
    if let Ok(dir) = std::env::var("HERDR_PLUGIN_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    if let Ok(root) = std::env::var("HERDR_PLUGIN_ROOT") {
        if !root.trim().is_empty() {
            return Some(PathBuf::from(root).join("config"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_file() {
        let cfg = load(Some("Z:/definitely/not/here/config.toml")).unwrap();
        assert!(cfg.auto_update);
        assert!(cfg.notify);
        assert!(cfg.exclude.is_empty());
        assert_eq!(cfg.timeout_secs, 20);
        assert_eq!(cfg.max_concurrency, 8);
        assert_eq!(cfg.policy, Policy::Auto);
        assert!(cfg.allow.is_empty());
        assert!(cfg.require_fast_forward);
        assert!(!cfg.allow_force_push);
    }

    #[test]
    fn parses_file() {
        let dir = std::env::temp_dir().join(format!("hau-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "auto_update = false\nnotify = false\nexclude = [\"flock.farm\", \"wave-tui.radio\"]\ntimeout_secs = 5\nmax_concurrency = 2\npolicy = \"pinned-only\"\nallow = [\"ragamo/*\"]\nrequire_fast_forward = false\nallow_force_push = true\n",
        )
        .unwrap();
        let cfg = load(Some(path.to_str().unwrap())).unwrap();
        assert!(!cfg.auto_update);
        assert!(!cfg.notify);
        assert_eq!(cfg.exclude, vec!["flock.farm", "wave-tui.radio"]);
        assert!(cfg.is_excluded("flock.farm"));
        assert!(!cfg.is_excluded("other"));
        assert_eq!(cfg.timeout_secs, 5);
        assert_eq!(cfg.max_concurrency, 2);
        assert_eq!(cfg.policy, Policy::PinnedOnly);
        assert!(cfg.is_allowed("ragamo", "herdr-flock"));
        assert!(!cfg.is_allowed("dio16", "herdr-flock"));
        assert!(!cfg.require_fast_forward);
        assert!(cfg.allow_force_push);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_dir_resolves() {
        let dir = std::env::temp_dir().join(format!("hau-cfgdir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        assert_eq!(config_dir(Some(path.to_str().unwrap())), Some(dir.clone()));
        assert_eq!(config_dir(None), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_policy_is_fatal() {
        let dir = std::env::temp_dir().join(format!("hau-cfg-pol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "policy = \"everywhere\"\n").unwrap();
        assert!(load(Some(path.to_str().unwrap())).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn glob_matches() {
        assert!(glob_match("ragamo/*", "ragamo/herdr-flock"));
        assert!(glob_match("*/herdr-flock", "ragamo/herdr-flock"));
        assert!(glob_match("*", "a/b"));
        assert!(glob_match("a?c/x", "abc/x"));
        assert!(!glob_match("ragamo/*", "dio16/herdr-flock"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn allow_empty_admits_all() {
        let cfg = Config::default();
        assert!(cfg.is_allowed("any", "thing"));
    }

    #[test]
    fn invalid_toml_is_fatal() {
        let dir = std::env::temp_dir().join(format!("hau-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "auto_update = [not a bool\n").unwrap();
        assert!(load(Some(path.to_str().unwrap())).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
