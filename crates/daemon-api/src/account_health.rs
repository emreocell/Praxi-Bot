//! Daily account/session health loop and owner notice for stale connectors.

use std::sync::Arc;

use account_health::{ComposioConnState, Deps, MailProbe, RunOutcome, report_due, run_once};
use channel_api::notice::{NoticeKind, NoticeSource};

use crate::composio::{ComposioConnState as ApiConn, ComposioControl};
use crate::email::EmailControl;
use crate::owner_notify::{Draft, Hub};

use crate::AppState;

const RECONNECT_COALESCE_MS: i64 = 24 * 3600 * 1000;

fn email_err(e: crate::email::EmailControlError) -> String {
    match e {
        crate::email::EmailControlError::NotFound(m) => m,
        crate::email::EmailControlError::Conflict(m) => m,
        crate::email::EmailControlError::Invalid(m) => m,
        crate::email::EmailControlError::Internal(m) => m,
        crate::email::EmailControlError::Upstream(m) => m,
    }
}

struct ComposioAdapter(Arc<dyn ComposioControl>);

impl account_health::ComposioProbe for ComposioAdapter {
    fn apps(&self) -> Vec<(String, String, ComposioConnState, bool)> {
        self.0
            .apps()
            .into_iter()
            .map(|a| (a.slug, a.name, map_conn(a.state), a.enabled))
            .collect()
    }

    fn refresh(
        &self,
        slug: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ComposioConnState> + Send + '_>> {
        let inner = Arc::clone(&self.0);
        let slug = slug.to_owned();
        Box::pin(async move {
            match inner.app(&slug).await {
                Ok(a) => map_conn(a.state),
                Err(_) => ComposioConnState::Failed,
            }
        })
    }
}

fn map_conn(s: ApiConn) -> ComposioConnState {
    match s {
        ApiConn::NotConnected => ComposioConnState::NotConnected,
        ApiConn::Pending => ComposioConnState::Pending,
        ApiConn::Active => ComposioConnState::Active,
        ApiConn::Expired => ComposioConnState::Expired,
        ApiConn::Failed => ComposioConnState::Failed,
    }
}

struct MailAdapter(Arc<dyn EmailControl>);

impl MailProbe for MailAdapter {
    fn accounts(&self) -> Vec<String> {
        self.0.list().into_iter().map(|a| a.name).collect()
    }

    fn test(
        &self,
        name: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        let inner = Arc::clone(&self.0);
        let name = name.to_owned();
        Box::pin(async move {
            inner
                .test_saved(&name)
                .await
                .map(|r| r.ok)
                .map_err(email_err)
        })
    }
}

fn publish_reconnect(hub: &Hub, lines: &[String], now_ms: i64) {
    if lines.is_empty() {
        return;
    }
    let mut d = Draft::new(
        NoticeKind::Blocked,
        NoticeSource::Run,
        "Reconnect accounts".into(),
    );
    d.reason = Some(format!(
        "{} connection(s) may have expired; sign in again in the app",
        lines.len()
    ));
    d.app_only = lines.iter().take(12).cloned().collect();
    d.coalesce = Some(("account_health:reconnect".into(), RECONNECT_COALESCE_MS));
    let _ = hub.publish(d, now_ms);
}

fn deps(state: &AppState) -> Deps {
    Deps {
        data_dir: state.data_dir(),
        composio: state.composio().map(|c| {
            Arc::new(ComposioAdapter(c)) as Arc<dyn account_health::ComposioProbe + Send + Sync>
        }),
        mail: state
            .email()
            .map(|e| Arc::new(MailAdapter(e)) as Arc<dyn MailProbe + Send + Sync>),
    }
}

/// Spawn the daily health loop (safe to call once at daemon start).
pub fn spawn_daily(state: Arc<AppState>) {
    if state.safe_mode() {
        return;
    }
    tokio::spawn(async move {
        let every = std::time::Duration::from_secs(24 * 3600);
        loop {
            let deps = deps(&state);
            if report_due(&deps.data_dir, std::time::SystemTime::now(), every) {
                let outcome = run_once(&deps).await;
                publish_reconnect(
                    state.owner_notify_hub(),
                    &outcome.notice_lines,
                    storage::now_ms(),
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
}

/// One synchronous pass (tests and manual trigger).
pub async fn run_now(state: &AppState) -> RunOutcome {
    let outcome = run_once(&deps(state)).await;
    publish_reconnect(
        state.owner_notify_hub(),
        &outcome.notice_lines,
        storage::now_ms(),
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use account_health::{ItemKind, ItemStatus, ProbeItem, StoredReport, save_report};
    use tempfile::tempdir;

    #[test]
    fn doctor_row_maps_from_store() {
        let dir = tempdir().unwrap();
        save_report(
            dir.path(),
            &StoredReport {
                finished_at_ms: 1,
                items: vec![ProbeItem {
                    kind: ItemKind::Mail,
                    key: "work".into(),
                    status: ItemStatus::Ok,
                    detail: String::new(),
                    bot_id: None,
                    site: None,
                }],
                reconnect: Vec::new(),
            },
        )
        .unwrap();
        let row = account_health::check_account_health(dir.path());
        assert_eq!(row.status, account_health::DoctorStatus::Ok);
    }
}
