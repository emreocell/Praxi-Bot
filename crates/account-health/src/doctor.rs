//! `bot doctor` row from the last stored report.

use std::path::Path;

use crate::report::{ItemStatus, load_report};

/// Plain check result for the embedder's doctor UI (id `account_health`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub id: &'static str,
    pub title: &'static str,
    pub status: DoctorStatus,
    pub detail: String,
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Skipped,
}

/// Builds the `account_health` doctor row from the last on-disk report.
#[must_use]
pub fn check_account_health(data_dir: &Path) -> DoctorRow {
    const ID: &str = "account_health";
    const TITLE: &str = "account sessions";
    const HINT: &str = "open Apps or the bot's browser profile to reconnect; see Settings → Diagnostics or run `bot doctor` after the daily check";
    let Some(report) = load_report(data_dir) else {
        return DoctorRow {
            id: ID,
            title: TITLE,
            status: DoctorStatus::Skipped,
            detail: "no daily account check has run yet".into(),
            hint: "the daemon runs one cheap probe per day when it is serving".into(),
        };
    };
    let (ok, bad, skip) = report.summarize();
    if bad == 0 {
        return DoctorRow {
            id: ID,
            title: TITLE,
            status: DoctorStatus::Ok,
            detail: if ok == 0 && skip == 0 {
                "no site ledger accounts or connectors to check".into()
            } else {
                format!("{ok} ok, {skip} skipped or unknown (last check recorded)")
            },
            hint: String::new(),
        };
    }
    let sample: Vec<_> = report
        .items
        .iter()
        .filter(|i| i.status == ItemStatus::Expired)
        .take(3)
        .map(|i| i.key.clone())
        .collect();
    let more = bad.saturating_sub(sample.len());
    let tail = if more == 0 {
        String::new()
    } else {
        format!(" and {more} more")
    };
    DoctorRow {
        id: ID,
        title: TITLE,
        status: DoctorStatus::Warn,
        detail: format!(
            "{bad} expired or signed out ({}{}); {ok} ok",
            sample.join(", "),
            tail
        ),
        hint: HINT.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ItemKind, ProbeItem, StoredReport, save_report};
    use tempfile::tempdir;

    #[test]
    fn warns_on_expired_items() {
        let dir = tempdir().unwrap();
        save_report(
            dir.path(),
            &StoredReport {
                finished_at_ms: 1,
                items: vec![ProbeItem {
                    kind: ItemKind::Composio,
                    key: "gmail".into(),
                    status: ItemStatus::Expired,
                    detail: String::new(),
                    bot_id: None,
                    site: None,
                }],
                reconnect: vec!["x".into()],
            },
        )
        .unwrap();
        let row = check_account_health(dir.path());
        assert_eq!(row.status, DoctorStatus::Warn);
        assert!(row.detail.contains("gmail"));
    }
}
