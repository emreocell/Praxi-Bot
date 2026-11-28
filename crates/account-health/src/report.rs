//! Persisted report the doctor and `site_ledger check` read.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fsutil::{MAX_READ, create_private_dir, read_capped, write_atomic};

pub const REPORT_FILE: &str = "account-health/report.json";
pub const STATE_FILE: &str = "account-health/state.json";
const MAX_ITEMS: usize = 500;
/// Two passes (the daemon's and a manual one) never write the two files at the same time.
static SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Text that came from a third party or a bot, made safe to show: control and direction
/// characters dropped, white space collapsed, at most `max` characters.
#[must_use]
pub fn clip_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = !out.is_empty();
            continue;
        }
        if matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        {
            continue;
        }
        if chars >= max {
            out.push('…');
            return out;
        }
        if gap {
            out.push(' ');
            chars += 1;
            gap = false;
            if chars >= max {
                out.push('…');
                return out;
            }
        }
        out.push(c);
        chars += 1;
    }
    out
}

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
    /// When this item was probed (0 = unknown).
    #[serde(default)]
    pub checked_at_ms: u64,
    /// Mail only: a hash of the account's non-secret settings when it was probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
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
    let bytes = read_capped(&report_path(data_dir), MAX_READ)?;
    serde_json::from_slice(&bytes).ok()
}

/// # Errors
/// I/O or serialization errors while writing the report.
pub fn save_report(data_dir: &Path, report: &StoredReport) -> std::io::Result<()> {
    let _guard = SAVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    create_private_dir(&data_dir.join("account-health"))?;
    let path = report_path(data_dir);
    let json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    write_atomic(&path, &json)?;
    let state = State {
        last_finished_at_ms: report.finished_at_ms,
    };
    let state_json = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    write_atomic(&state_path(data_dir), &state_json)?;
    Ok(())
}

#[must_use]
pub fn due(data_dir: &Path, now: SystemTime, interval: Duration) -> bool {
    let path = state_path(data_dir);
    let Some(bytes) = read_capped(&path, MAX_READ) else {
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
    pub fn status_for_site(&self, bot: &str, site: &str) -> Option<&ProbeItem> {
        self.items.iter().find(|i| {
            i.kind == ItemKind::LedgerSite
                && i.bot_id.as_deref() == Some(bot)
                && i.site.as_deref() == Some(site)
        })
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
                checked_at_ms: 0,
                fingerprint: None,
            }],
            reconnect: Vec::new(),
        };
        save_report(dir.path(), &report).unwrap();
        let got = load_report(dir.path()).unwrap();
        assert_eq!(got, report);
    }

    fn site_item(bot: &str, site: &str, status: ItemStatus) -> ProbeItem {
        ProbeItem {
            kind: ItemKind::LedgerSite,
            key: format!("{bot}/{site}"),
            status,
            detail: String::new(),
            bot_id: Some(bot.into()),
            site: Some(site.into()),
            checked_at_ms: 0,
            fingerprint: None,
        }
    }

    #[test]
    fn site_status_never_crosses_bots() {
        let report = StoredReport {
            finished_at_ms: 1,
            items: vec![
                site_item("bot-a", "example.com", ItemStatus::Expired),
                site_item("bot-b", "other.com", ItemStatus::Ok),
            ],
            reconnect: Vec::new(),
        };
        assert!(report.status_for_site("bot-a", "example.com").is_some());
        assert!(report.status_for_site("bot-b", "example.com").is_none());
        assert!(report.status_for_site("bot-a", "other.com").is_none());
    }

    #[test]
    fn clip_text_strips_controls_and_clips() {
        assert_eq!(clip_text("a\n\tb \u{202E}c\0d", 80), "a b c d");
        assert_eq!(clip_text("  hi  ", 80), "hi");
        let long = "x".repeat(500);
        let out = clip_text(&long, 60);
        assert_eq!(out.chars().count(), 61);
        assert!(out.ends_with('…'));
    }

    #[cfg(unix)]
    #[test]
    fn saved_report_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempdir().unwrap();
        save_report(dir.path(), &StoredReport::default()).unwrap();
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir.path().join("account-health")), 0o700);
        assert_eq!(mode(report_path(dir.path())), 0o600);
        assert_eq!(mode(state_path(dir.path())), 0o600);
    }
}
