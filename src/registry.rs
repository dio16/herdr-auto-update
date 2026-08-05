// Installed-plugin registry access through the herdr CLI
// (`herdr plugin list --json`), which is the authoritative source and works
// on every platform without guessing data-dir paths.

use serde::Deserialize;
use std::process::Command;

pub const GITHUB_KIND: &str = "github";

#[derive(Debug, Clone, Deserialize)]
pub struct Plugin {
    #[serde(rename = "plugin_id")]
    pub plugin_id: String,
    #[serde(default)]
    pub source: Option<Source>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub kind: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub requested_ref: Option<String>,
    #[serde(default, rename = "resolved_commit")]
    pub resolved_commit: Option<String>,
}

#[derive(Deserialize)]
struct PluginList {
    result: PluginListResult,
}

#[derive(Deserialize)]
struct PluginListResult {
    plugins: Vec<Plugin>,
}

/// herdr binary: HERDR_BIN_PATH is injected when herdr runs plugin commands;
/// fall back to PATH lookup for standalone CLI use.
pub fn herdr_bin() -> String {
    env_nonempty("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".to_string())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn list_installed() -> Result<Vec<Plugin>, String> {
    let bin = herdr_bin();
    let out = Command::new(&bin)
        .args(["plugin", "list", "--json"])
        .output()
        .map_err(|e| format!("cannot run '{bin} plugin list --json': {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "'{bin} plugin list --json' exited with {}: {}",
            out.status,
            err.trim()
        ));
    }
    let text = decode_stdout(&out.stdout);
    let parsed: PluginList = serde_json::from_str(&text)
        .map_err(|e| format!("cannot parse '{bin} plugin list --json' output: {e}"))?;
    Ok(parsed.result.plugins)
}

/// herdr may write UTF-16LE to a console pipe; detect the BOM and decode.
fn decode_stdout(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let mut units = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes[2..].chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Validate a GitHub owner/repo segment: alphanumeric plus `-`, `_`, `.`,
/// bounded length. Blocks argv/URL injection from a tampered registry.
pub fn valid_github_name(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_accepted() {
        for name in [
            "ragamo",
            "herdr-flock",
            "herdr-file-viewer",
            "a.b_c-d",
            "A1",
        ] {
            assert!(valid_github_name(name, 100), "should accept {name:?}");
        }
    }

    #[test]
    fn invalid_names_rejected() {
        for name in [
            "",
            "bad;rm -rf",
            "a/b",
            "x y",
            "evil$IFS",
            "..",
            "a\nb",
            "a@b",
        ] {
            assert!(!valid_github_name(name, 100), "should reject {name:?}");
        }
    }

    #[test]
    fn length_bounded() {
        assert!(valid_github_name("a", 1));
        assert!(!valid_github_name("ab", 1));
    }

    #[test]
    fn decodes_utf8() {
        assert_eq!(decode_stdout(b"{\"a\":1}"), "{\"a\":1}");
        assert_eq!(decode_stdout("日本語".as_bytes()), "日本語");
    }

    #[test]
    fn decodes_utf16le_with_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "{\"x\":\"日本\"}".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_stdout(&bytes), "{\"x\":\"日本\"}");
    }
}
