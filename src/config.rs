// Plugin configuration, read from HERDR_PLUGIN_CONFIG_DIR/config.toml
// (herdr creates that directory on install) with CLI override support.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Reinstall outdated plugins automatically from the startup hook.
    pub auto_update: bool,
    /// Plugin ids to skip during updates.
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_update: true,
            exclude: Vec::new(),
        }
    }
}

impl Config {
    pub fn is_excluded(&self, plugin_id: &str) -> bool {
        self.exclude.iter().any(|e| e == plugin_id)
    }
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
    toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_file() {
        let cfg = load(Some("Z:/definitely/not/here/config.toml")).unwrap();
        assert!(cfg.auto_update);
        assert!(cfg.exclude.is_empty());
    }

    #[test]
    fn parses_file() {
        let dir = std::env::temp_dir().join(format!("hau-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "auto_update = false\nexclude = [\"flock.farm\", \"wave-tui.radio\"]\n",
        )
        .unwrap();
        let cfg = load(Some(path.to_str().unwrap())).unwrap();
        assert!(!cfg.auto_update);
        assert_eq!(cfg.exclude, vec!["flock.farm", "wave-tui.radio"]);
        assert!(cfg.is_excluded("flock.farm"));
        assert!(!cfg.is_excluded("other"));
        std::fs::remove_dir_all(&dir).ok();
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
