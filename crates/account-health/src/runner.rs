//! One full probe pass and the daily background loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::ledger::{probe_ledger_target, scan_ledger_targets};
use crate::report::{
    ItemKind, ItemStatus, ProbeItem, StoredReport, cap_items, due, now_ms, save_report,
};

const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
const LOOP_TICK: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposioConnState {
    NotConnected,
    Pending,
    Active,
    Expired,
    Failed,
}

/// Host hooks the daemon supplies (no secrets in reports).
pub struct Deps {
    pub data_dir: PathBuf,
    pub composio: Option<Arc<dyn ComposioProbe + Send + Sync>>,
    pub mail: Option<Arc<dyn MailProbe + Send + Sync>>,
}

/// Host hook for Composio app authorization checks.
pub trait ComposioProbe: Send + Sync {
    fn apps(&self) -> Vec<(String, String, ComposioConnState, bool)>;
    fn refresh(
        &self,
        slug: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ComposioConnState> + Send + '_>>;
}

pub trait MailProbe: Send + Sync {
    fn accounts(&self) -> Vec<String>;
    fn test(
        &self,
        name: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutcome {
    pub report: StoredReport,
    pub notice_lines: Vec<String>,
}

fn mail_item(name: &str, ok: bool, detail: String) -> ProbeItem {
    ProbeItem {
        kind: ItemKind::Mail,
        key: name.to_owned(),
        status: if ok {
            ItemStatus::Ok
        } else if detail.contains("auth") || detail.contains("credential") {
            ItemStatus::Expired
        } else {
            ItemStatus::Unknown
        },
        detail: if ok { String::new() } else { detail },
        bot_id: None,
        site: None,
    }
}

fn composio_item(slug: &str, name: &str, state: ComposioConnState) -> ProbeItem {
    let status = match state {
        ComposioConnState::Active => ItemStatus::Ok,
        ComposioConnState::Expired | ComposioConnState::Failed => ItemStatus::Expired,
        ComposioConnState::Pending | ComposioConnState::NotConnected => ItemStatus::Skipped,
    };
    ProbeItem {
        kind: ItemKind::Composio,
        key: slug.to_owned(),
        status,
        detail: if status == ItemStatus::Ok {
            String::new()
        } else {
            format!("{name}: {}", composio_state_label(state))
        },
        bot_id: None,
        site: None,
    }
}

fn composio_state_label(state: ComposioConnState) -> &'static str {
    match state {
        ComposioConnState::NotConnected => "not connected",
        ComposioConnState::Pending => "sign-in pending",
        ComposioConnState::Active => "active",
        ComposioConnState::Expired => "authorization expired",
        ComposioConnState::Failed => "sign-in failed",
    }
}

fn reconnect_line(item: &ProbeItem) -> Option<String> {
    if item.status != ItemStatus::Expired {
        return None;
    }
    Some(match item.kind {
        ItemKind::LedgerSite => format!(
            "Bot {} on {} (account {}): {}",
            item.bot_id.as_deref().unwrap_or("?"),
            item.site.as_deref().unwrap_or("?"),
            item.key,
            item.detail
        ),
        ItemKind::Mail => format!("Mail identity {}: {}", item.key, item.detail),
        ItemKind::Composio => format!("Composio app {}: {}", item.key, item.detail),
    })
}

/// Run every probe once (async connectors, blocking ledger/HTTP on workers).
pub async fn run_once(deps: &Deps) -> RunOutcome {
    let data_dir = &deps.data_dir;
    let mut items = Vec::new();
    let targets = scan_ledger_targets(data_dir);
    let ledger_items: Vec<_> = tokio::task::spawn_blocking({
        let data_dir = data_dir.clone();
        move || {
            targets
                .iter()
                .map(|t| probe_ledger_target(&data_dir, t))
                .collect()
        }
    })
    .await
    .unwrap_or_default();
    items.extend(ledger_items);

    if let Some(mail) = &deps.mail {
        for name in mail.accounts() {
            let item = match mail.test(&name).await {
                Ok(true) => mail_item(&name, true, String::new()),
                Ok(false) => mail_item(&name, false, "mail login failed".into()),
                Err(e) => ProbeItem {
                    kind: ItemKind::Mail,
                    key: name,
                    status: ItemStatus::Unknown,
                    detail: e,
                    bot_id: None,
                    site: None,
                },
            };
            items.push(item);
        }
    }

    if let Some(composio) = &deps.composio {
        for (slug, name, state, enabled) in composio.apps() {
            if !enabled {
                continue;
            }
            let mut state = state;
            if !matches!(state, ComposioConnState::Active) {
                state = composio.refresh(&slug).await;
            }
            items.push(composio_item(&slug, &name, state));
        }
    }

    items = cap_items(items);
    let reconnect: Vec<String> = items.iter().filter_map(reconnect_line).collect();
    let report = StoredReport {
        finished_at_ms: now_ms(),
        items,
        reconnect: reconnect.clone(),
    };
    if let Err(e) = save_report(data_dir, &report) {
        tracing::warn!(error = %e, "account health report was not saved");
    }
    RunOutcome {
        report,
        notice_lines: reconnect,
    }
}

/// Background loop: at most one pass per 24 hours.
pub async fn daily_loop(deps: Deps) {
    loop {
        let now = SystemTime::now();
        if due(&deps.data_dir, now, CHECK_EVERY) {
            let outcome = run_once(&deps).await;
            if !outcome.notice_lines.is_empty() {
                tracing::info!(
                    count = outcome.notice_lines.len(),
                    "account health: sessions need reconnect"
                );
            }
        }
        tokio::time::sleep(LOOP_TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_run_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let deps = Deps {
            data_dir: dir.path().to_path_buf(),
            composio: None,
            mail: None,
        };
        let out = run_once(&deps).await;
        assert_eq!(out.report.items.len(), 0);
        assert!(crate::load_report(dir.path()).is_some());
    }
}
