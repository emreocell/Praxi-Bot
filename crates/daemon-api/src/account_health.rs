//! Daily account/session health loop and owner notice for stale connectors.

use std::sync::Arc;

use account_health::{
    ComposioConnState, Deps, MailOutcome, MailProbe, RunOutcome, clip_text, report_due, run_once,
};
use channel_api::notice::{NoticeKind, NoticeSource};

use crate::composio::{ComposioConnState as ApiConn, ComposioControl};
use crate::email::{EmailAccountDto, EmailControl, EmailProbeStatus, EmailTestReport};
use crate::owner_notify::{Draft, Hub};

use crate::AppState;

const RECONNECT_COALESCE_MS: i64 = 24 * 3600 * 1000;
const EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const TICK: std::time::Duration = std::time::Duration::from_secs(3600);
/// The first pass waits, so it does not race the connectors' own start-up refresh.
const STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const STARTUP_JITTER_SECS: u64 = 5 * 60;
const NOTICE_LINE_CAP: usize = 240;

fn email_err(e: crate::email::EmailControlError) -> String {
    use crate::email::EmailControlError as E;
    match e {
        E::NotFound(m) | E::Conflict(m) | E::Invalid(m) | E::Internal(m) | E::Upstream(m) => m,
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

    fn check(
        &self,
        slug: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ComposioConnState>> + Send + '_>>
    {
        let inner = Arc::clone(&self.0);
        let slug = slug.to_owned();
        Box::pin(async move { peeked(inner.peek_app_state(&slug).await) })
    }
}

/// An answer from Composio is a state; offline, a rate limit or a host error is "could not
/// tell", never an expired session.
fn peeked(r: Result<ApiConn, crate::composio::ComposioControlError>) -> Option<ComposioConnState> {
    r.ok().map(map_conn)
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

/// Only a server that rejected the credentials counts as a bad login.
fn mail_outcome(r: EmailTestReport) -> MailOutcome {
    if r.ok {
        return MailOutcome::Ok;
    }
    let rejected = |s: EmailProbeStatus| s == EmailProbeStatus::AuthFailed;
    if rejected(r.imap) || rejected(r.smtp) || r.api.is_some_and(rejected) {
        MailOutcome::AuthFailed
    } else {
        MailOutcome::Transient("the mail server could not be reached or did not answer".into())
    }
}

/// FNV-1a over the account's non-secret settings (stable across builds).
fn settings_hash(a: &EmailAccountDto) -> String {
    let text = format!(
        "{:?}|{}|{}|{}|{:?}|{:?}|{:?}|{}",
        a.provider,
        a.address,
        a.username,
        a.imap.host,
        a.imap.port,
        a.smtp,
        a.inbox_id,
        a.custom_ca
    );
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

impl MailProbe for MailAdapter {
    fn accounts(&self) -> Vec<String> {
        self.0.list().into_iter().map(|a| a.name).collect()
    }

    fn fingerprint(&self, name: &str) -> Option<String> {
        self.0
            .list()
            .iter()
            .find(|a| a.name == name)
            .map(settings_hash)
    }

    fn test(
        &self,
        name: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = MailOutcome> + Send + '_>> {
        let inner = Arc::clone(&self.0);
        let name = name.to_owned();
        Box::pin(async move {
            match inner.test_saved(&name).await {
                Ok(report) => mail_outcome(report),
                Err(e) => MailOutcome::Transient(email_err(e)),
            }
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
    d.app_only = lines
        .iter()
        .take(12)
        .map(|l| clip_text(l, NOTICE_LINE_CAP))
        .collect();
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
///
/// The loop remembers when it last ran, so a report that cannot be written does not turn the
/// daily pass into an hourly one.
pub fn spawn_daily(state: AppState) {
    if state.safe_mode() {
        return;
    }
    tokio::spawn(async move {
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::from(d.subsec_nanos()) % STARTUP_JITTER_SECS);
        tokio::time::sleep(STARTUP_DELAY + std::time::Duration::from_secs(jitter)).await;
        let mut last_run: Option<std::time::Instant> = None;
        loop {
            let deps = deps(&state);
            let waiting = last_run.is_some_and(|t| t.elapsed() < EVERY);
            if !waiting && report_due(&deps.data_dir, std::time::SystemTime::now(), EVERY) {
                last_run = Some(std::time::Instant::now());
                let outcome = run_once(&deps).await;
                publish_reconnect(
                    state.owner_notify(),
                    &outcome.notice_lines,
                    storage::now_ms(),
                );
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// One synchronous pass (tests and manual trigger).
pub async fn run_now(state: &AppState) -> RunOutcome {
    let outcome = run_once(&deps(state)).await;
    publish_reconnect(
        state.owner_notify(),
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
                    checked_at_ms: 0,
                    fingerprint: None,
                }],
                reconnect: Vec::new(),
            },
        )
        .unwrap();
        let row = account_health::check_account_health(dir.path());
        assert_eq!(row.status, account_health::DoctorStatus::Ok);
    }

    fn report(ok: bool, imap: EmailProbeStatus, smtp: EmailProbeStatus) -> EmailTestReport {
        EmailTestReport {
            imap,
            smtp,
            ok,
            api: None,
        }
    }

    #[test]
    fn only_rejected_credentials_are_an_auth_failure() {
        use EmailProbeStatus::{AuthFailed, Ok as Fine, Timeout, Unreachable};
        assert_eq!(mail_outcome(report(true, Fine, Fine)), MailOutcome::Ok);
        assert_eq!(
            mail_outcome(report(false, AuthFailed, Fine)),
            MailOutcome::AuthFailed
        );
        assert_eq!(
            mail_outcome(report(false, Fine, AuthFailed)),
            MailOutcome::AuthFailed
        );
        assert!(matches!(
            mail_outcome(report(false, Unreachable, Timeout)),
            MailOutcome::Transient(_)
        ));
        let mut api = report(false, Fine, Fine);
        api.api = Some(AuthFailed);
        assert_eq!(mail_outcome(api), MailOutcome::AuthFailed);
    }

    #[test]
    fn composio_errors_mean_could_not_tell() {
        use crate::composio::ComposioControlError as E;
        assert_eq!(
            peeked(Ok(ApiConn::Expired)),
            Some(ComposioConnState::Expired)
        );
        assert_eq!(peeked(Ok(ApiConn::Active)), Some(ComposioConnState::Active));
        for e in [
            E::RateLimited("slow down".into()),
            E::Upstream("offline".into()),
            E::LoginExpired,
            E::NotConfigured,
        ] {
            assert_eq!(peeked(Err(e)), None);
        }
    }
}
