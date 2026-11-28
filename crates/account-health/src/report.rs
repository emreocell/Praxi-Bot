//! Persisted report the doctor and `site_ledger check` read.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const REPORT_FILE: &str = "account-health/report.json";
pub const STATE_FILE: &str = "account-health/state.json";
const MAX_ITEMS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    LedgerSite,
    Mail,
    Composio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Ok,
    Expired,
    Unknown,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeItem {
    pub kind: ItemKind,
    pub key: String,
    pub status: ItemStatus,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredReport {
    pub finished_at_ms: u64,
    #[serde(default)]
    pub items: Vec<ProbeItem>,
    #[serde(default)]
    pub reconnect: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    last_finished_at_ms: u64,
}

#[must_use]
pub fn report_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REPORT_FILE)
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[must_use]
pub fn load_report(data_dir: &Path) -> Option<StoredReport> {
    let bytes = std::fs::read(report_path(data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// # Errors
/// I/O or serialization errors while writing the report.
pub fn save_report(data_dir: &Path, report: &StoredReport) -> std::io::Result<()> {
    let dir = data_dir.join("account-health");
    std::fs::create_dir_all(&dir)?;
    let path = report_path(data_dir);
    let json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    crate::ledger::write_atomic(&path, &json)?;
    let state = State {
        last_finished_at_ms: report.finished_at_ms,
    };
    let state_json = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    crate::ledger::write_atomic(&state_path(data_dir), &state_json)?;
    Ok(())
}

#[must_use]
pub fn due(data_dir: &Path, now: SystemTime, interval: Duration) -> bool {
    let path = state_path(data_dir);
    let Ok(bytes) = std::fs::read(path) else {
        return true;
    };
    let Ok(state) = serde_json::from_slice::<State>(&bytes) else {
        return true;
    };
    let Ok(elapsed) = now.duration_since(UNIX_EPOCH) else {
        return true;
    };
    let now_ms = u64::try_from(elapsed.as_millis()).unwrap_or(0);
    now_ms.saturating_sub(state.last_finished_at_ms)
        >= u64::try_from(interval.as_millis()).unwrap_or(u64::MAX)
}

impl StoredReport {
    #[must_use]
    pub fn summarize(&self) -> (usize, usize, usize) {
        let mut ok = 0usize;
        let mut bad = 0usize;
        let mut skip = 0usize;
        for i in &self.items {
            match i.status {
                ItemStatus::Ok => ok += 1,
                ItemStatus::Expired => bad += 1,
                ItemStatus::Skipped | ItemStatus::Unknown => skip += 1,
            }
        }
        (ok, bad, skip)
    }

    #[must_use]
    pub fn status_for_site(&self, site: &str) -> Option<&ProbeItem> {
        self.items
            .iter()
            .find(|i| i.kind == ItemKind::LedgerSite && i.site.as_deref() == Some(site))
    }
}

#[must_use]
pub fn cap_items(mut items: Vec<ProbeItem>) -> Vec<ProbeItem> {
    if items.len() > MAX_ITEMS {
        items.truncate(MAX_ITEMS);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn due_is_true_when_never_run() {
        let dir = tempdir().unwrap();
        assert!(due(
            dir.path(),
            SystemTime::now(),
            Duration::from_secs(3600)
        ));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let report = StoredReport {
            finished_at_ms: now_ms(),
            items: vec![ProbeItem {
                kind: ItemKind::Mail,
                key: "work".into(),
                status: ItemStatus::Ok,
                detail: String::new(),
                bot_id: None,
                site: None,
            }],
            reconnect: Vec::new(),
        };
        save_report(dir.path(), &report).unwrap();
        let got = load_report(dir.path()).unwrap();
        assert_eq!(got, report);
    }
}
