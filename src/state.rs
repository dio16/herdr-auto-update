// Update/rollback audit trail (`state.json` in the plugin config dir).
// This is the one file the plugin deliberately writes (§5.8): rollback needs
// to know the previous commit of every update. Entries are append-only;
// the most recent entry per plugin is the rollback target.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    pub plugin_id: String,
    pub previous_sha: String,
    pub current_sha: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    pub updated_at: String,
    /// "updated" | "rolled_back"
    pub result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub entries: Vec<StateEntry>,
}

const STATE_VERSION: u32 = 1;

impl State {
    /// Load state.json; missing/corrupt file yields an empty state (rollback
    /// then reports "no history" instead of failing on a bad file).
    pub fn load(dir: &Path) -> State {
        let text = match std::fs::read_to_string(dir.join(STATE_FILE)) {
            Ok(t) => t,
            Err(_) => return State::default(),
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Best-effort persist. Errors are reported to the caller (stderr), but
    /// a failed state write must not change update semantics.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize state: {e}"))?;
        std::fs::write(dir.join(STATE_FILE), text)
            .map_err(|e| format!("cannot write {}: {e}", STATE_FILE))
    }

    pub fn append(&mut self, e: StateEntry) {
        self.version = STATE_VERSION;
        self.entries.push(e);
    }

    /// Most recent entry for a plugin id (rollback target).
    pub fn latest_for(&self, plugin_id: &str) -> Option<&StateEntry> {
        self.entries.iter().rev().find(|e| e.plugin_id == plugin_id)
    }
}

/// RFC 3339 UTC timestamp without external deps (Howard Hinnant's
/// civil_from_days).
pub fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_epoch(secs)
}

/// Pure formatter for a unix timestamp (exported for tests).
fn format_epoch(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_epochs() {
        assert_eq!(rfc3339_now().len(), 20);
        // 1700000000 = 2023-11-14T22:13:20Z (known constant).
        assert_eq!(format_epoch(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_epoch(86_400), "1970-01-02T00:00:00Z");
        // 1664455200 = 2022-09-29T12:40:00Z (verified via epoch arithmetic:
        // 19264 days after 1970-01-01 = 2022-09-29, remainder 45600s).
        assert_eq!(format_epoch(1_664_455_200), "2022-09-29T12:40:00Z");
    }

    #[test]
    fn state_roundtrip_and_latest() {
        let dir = std::env::temp_dir().join(format!("hau-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = State::default();
        s.append(StateEntry {
            plugin_id: "p1".into(),
            previous_sha: "a".repeat(40),
            current_sha: "b".repeat(40),
            requested_ref: None,
            updated_at: "t1".into(),
            result: "updated".into(),
        });
        s.append(StateEntry {
            plugin_id: "p1".into(),
            previous_sha: "b".repeat(40),
            current_sha: "c".repeat(40),
            requested_ref: None,
            updated_at: "t2".into(),
            result: "updated".into(),
        });
        s.save(&dir).unwrap();
        let loaded = State::load(&dir);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            loaded.latest_for("p1").unwrap().previous_sha,
            "b".repeat(40)
        );
        assert!(loaded.latest_for("nope").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_state_is_empty() {
        let dir = std::env::temp_dir().join(format!("hau-state-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STATE_FILE), "{{{").unwrap();
        assert!(State::load(&dir).entries.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
