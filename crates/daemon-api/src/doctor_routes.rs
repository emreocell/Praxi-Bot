//! `/v1/doctor*`: read-only diagnostics, allow-listed one-click repairs and a
//! redacted support report (see [`crate::doctor`]).
//!
//! The checks run concurrently on blocking threads, each under a 3 s budget,
//! all under a 10 s deadline; a check that does not answer in time is reported
//! as a warning instead of holding the request.  Coding CLIs are detected once
//! per request (through the daemon's cached detector, on a blocking thread:
//! detection touches the file system) and the result is shared by every check
//! that needs it.
//!
//! One diagnostics run at a time: the run's permit is shared by every blocking
//! job it started and is released only when the last of them has really
//! ended, so a check stuck on a dead mount or keyring makes later requests get
//! `429` instead of piling up more stuck threads.  The support report has an
//! end-to-end deadline.
//!
//! Every route here is host-local: the peer router refuses `/v1/doctor*` to
//! every paired device role (see `crate::peer`).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use utoipa::ToSchema;

use crate::AppState;
use crate::doctor::{self, CapabilityStatus, Check, LinkPeer, LinkRuntime, Repair};
use crate::error::{ApiError, ErrorBody};
use crate::live_view::Actor;

/// Budget of one check.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// Budget of a whole `GET /v1/doctor`.
pub(crate) const OVERALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget of a whole `GET /v1/doctor/report` (checks plus redaction).
pub(crate) const REPORT_TIMEOUT: Duration = Duration::from_secs(15);

/// The single diagnostics slot, shared by a run and every blocking job it
/// started (released when the last holder drops it).
pub(crate) type RunPermit = Arc<tokio::sync::OwnedSemaphorePermit>;

/// What the embedder adds to the diagnostics (call
/// [`AppState::set_doctor_host`] once).
#[async_trait::async_trait]
pub trait DoctorHost: Send + Sync {
    /// Capabilities of the desktop automation backend, `None` when the daemon
    /// runs without one.
    fn desktop(&self) -> Option<desktop_api::Capabilities> {
        None
    }

    /// Whether [`DoctorHost::restart_browser`] is supported.
    fn can_restart_browser(&self) -> bool {
        false
    }

    /// Restart the shared browser session.
    ///
    /// # Errors
    /// Why it could not be restarted.
    async fn restart_browser(&self) -> Result<String, String> {
        Err("restarting the browser is not supported by this daemon".into())
    }

    /// The daemon's live secret redactor (stored secrets, the API token, the
    /// vault passphrase and credential environment variables, each with its
    /// encoded forms).  The support report uses it; without one the report
    /// builds an equivalent from the secret store and fails closed when the
    /// store cannot be read.
    fn redactor(&self) -> Option<agent_core::SharedRedactor> {
        None
    }
}

/// `GET /v1/doctor` response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DoctorReport {
    /// `false` when any check failed.
    pub ok: bool,
    /// Results, in display order.
    pub checks: Vec<Check>,
}

/// `POST /v1/doctor/repair/{id}` response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepairResult {
    /// The repair that ran.
    pub repair: Repair,
    /// What it did.
    pub detail: String,
}

/// One check to run: id, title and the blocking probe.
pub(crate) type Job = (
    &'static str,
    &'static str,
    Box<dyn FnOnce() -> Check + Send>,
);

/// Run `jobs` concurrently: each gets `per`, all of them end by `deadline`.
/// Results keep the order of `jobs`.  Every job holds a clone of `permit`
/// until it really ends, also after its result was given up on.
pub(crate) async fn run_jobs(
    jobs: Vec<Job>,
    per: Duration,
    deadline: Instant,
    permit: &RunPermit,
) -> Vec<Check> {
    let handles: Vec<_> = jobs
        .into_iter()
        .map(|(id, title, f)| {
            let until = (Instant::now() + per).min(deadline);
            let hold = Arc::clone(permit);
            let task = tokio::task::spawn_blocking(move || {
                let _hold = hold;
                f()
            });
            (id, title, until, task)
        })
        .collect();
    let mut out = Vec::with_capacity(handles.len());
    for (id, title, until, task) in handles {
        // A blocking thread cannot be cancelled: a late check finishes in the
        // background (still holding the permit) and its result is dropped.
        out.push(match tokio::time::timeout_at(until, task).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => Check::warn(
                id,
                title,
                format!("check crashed: {e}"),
                "report this as a bug",
            ),
            Err(_) => Check::warn(
                id,
                title,
                "did not answer in time",
                "run `bot doctor` in a terminal for the full check",
            ),
        });
    }
    out
}

/// Run the future `make()` returns on a blocking thread (it may do
/// synchronous I/O before it yields, which must not stall an async worker)
/// and wait for it until `until`.  The thread holds `permit` until it ends.
/// `None` when it did not answer in time or panicked.
pub(crate) async fn on_blocking_pool<T, Fut>(
    make: impl FnOnce() -> Fut + Send + 'static,
    until: Instant,
    permit: &RunPermit,
) -> Option<T>
where
    Fut: std::future::Future<Output = T>,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    let hold = Arc::clone(permit);
    let task = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        handle.block_on(make())
    });
    tokio::time::timeout_at(until, task).await.ok()?.ok()
}

/// The daemon's live pairing / connection state (what `GET /v1/host` shows).
fn link_runtime(s: &AppState) -> LinkRuntime {
    let Some(net) = s.leader().network() else {
        return LinkRuntime::NetworkDisabled;
    };
    LinkRuntime::Network(
        net.devices()
            .into_iter()
            .filter_map(|d| {
                let key = peer_network::key_from_string(&d.id).ok()?;
                Some(LinkPeer {
                    connected: net.is_connected(&key),
                    last_seen_secs: net.last_seen(&key).map(|t| t.elapsed().as_secs()),
                    key: d.id,
                    role: d.role.to_string(),
                })
            })
            .collect(),
    )
}

fn capabilities(desktop: Option<&desktop_api::Capabilities>) -> Vec<CapabilityStatus> {
    let has_display = browser_cdp::host::has_display();
    vec![
        CapabilityStatus {
            id: "display".into(),
            available: has_display,
            reason: (!has_display).then(|| "no display (headless)".into()),
        },
        CapabilityStatus {
            id: "desktop_automation".into(),
            available: desktop.is_some_and(|d| d.has_display),
            reason: match desktop {
                None => Some("not available on this platform".into()),
                Some(d) if !d.has_display => Some("no display".into()),
                Some(_) => None,
            },
        },
    ]
}

/// All daemon-side checks, ending by `deadline`.
#[allow(clippy::too_many_lines)] // The check table of the daemon, in display order.
pub(crate) async fn run_checks(s: &AppState, deadline: Instant, permit: &RunPermit) -> Vec<Check> {
    let start = Instant::now();
    // One CLI detection per request (the detector caches it for 60 s).
    let detector = s.detector();
    #[cfg(test)]
    let hook = s.0.doctor_detect_hook.lock().clone();
    let detected = on_blocking_pool(
        move || async move {
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            detector.detect(false).await
        },
        (start + CHECK_TIMEOUT).min(deadline),
        permit,
    )
    .await
    .map(Arc::new);
    let data_dir = s.data_dir();
    let has_catalog = s.provider_host().is_some();
    let store = s.secret_store();
    let active = s.agent().providers().ids();
    let host = s.doctor_host();
    let desktop = host.as_ref().and_then(|h| h.desktop());
    let can_restart = host.as_ref().is_some_and(|h| h.can_restart_browser());
    let version = env!("CARGO_PKG_VERSION");
    let uptime = s.uptime_secs();

    let mut jobs: Vec<Job> = Vec::new();
    let mut add = |id: &'static str, title: &'static str, f: Box<dyn FnOnce() -> Check + Send>| {
        jobs.push((id, title, f));
    };
    add(
        "daemon",
        "daemon",
        Box::new(move || {
            Check::ok(
                "daemon",
                "daemon",
                format!(
                    "running (v{version}, API {}), up {uptime} s",
                    crate::API_VERSION
                ),
            )
        }),
    );
    let d = data_dir.clone();
    add(
        "host_ownership",
        "host ownership",
        Box::new(move || doctor::check_host(&d)),
    );
    let (d, app) = (data_dir.clone(), s.clone());
    add(
        "peer_link",
        "peer link",
        Box::new(move || doctor::check_peer_link(&d, &link_runtime(&app))),
    );
    let d = data_dir.clone();
    add(
        "data_dir",
        "data dir",
        Box::new(move || doctor::check_data_dir(&d)),
    );
    let d = data_dir.clone();
    add(
        "unicode_path",
        "unicode paths",
        Box::new(move || doctor::check_unicode_path(&d)),
    );
    let d = data_dir.clone();
    add(
        "config",
        "config",
        Box::new(move || doctor::check_config(&d)),
    );
    let st = store.clone();
    add(
        "secret_backend",
        "secret backend",
        Box::new(move || match st {
            Some(st) => doctor::check_secrets(&Ok((st.as_ref(), "daemon store".into()))),
            None => Check::skipped(
                "secret_backend",
                "secret backend",
                "this daemon has no secret store",
            ),
        }),
    );
    let (d, app, st, det) = (data_dir.clone(), s.clone(), store.clone(), detected.clone());
    add(
        "providers",
        "providers",
        Box::new(move || {
            let empty = provider_catalog::Catalog::from_entries([]);
            let st_ref: doctor::StoreRef<'_> = match &st {
                Some(st) => Ok((st.as_ref(), "daemon store".into())),
                None => Err("this daemon has no secret store".into()),
            };
            doctor::check_providers(&doctor::ProviderInputs {
                data_dir: &d,
                catalog: Ok(app.provider_host().map_or(&empty, |h| &h.catalog)),
                store: &st_ref,
                detected: det.as_deref().map(Vec::as_slice),
                online: None,
            })
        }),
    );
    let d = data_dir.clone();
    add(
        "providers_loaded",
        "loaded providers",
        Box::new(move || doctor::check_providers_loaded(&d, &active)),
    );
    let det = detected.clone();
    add(
        "cli_accounts",
        "cli accounts",
        Box::new(move || doctor::check_cli_accounts(det.as_deref().map(Vec::as_slice))),
    );
    if has_catalog {
        let (d, app) = (data_dir.clone(), s.clone());
        add(
            "model_catalog",
            "model catalog",
            Box::new(move || {
                let empty = provider_catalog::Catalog::from_entries([]);
                let cat = app.provider_host().map_or(&empty, |h| &h.catalog);
                doctor::check_catalog(&d, &Ok(cat))
            }),
        );
    }
    add(
        "catalog_freshness",
        "catalog freshness",
        Box::new(|| doctor::check_catalog_freshness(SystemTime::now())),
    );
    add(
        "browser",
        "browser",
        Box::new(move || {
            let found = browser_cdp::find_browser();
            let c = match &found {
                Ok(p) => doctor::check_browser_found(Ok(p)),
                Err(e) => doctor::check_browser_found(Err(&e.to_string())),
            };
            if can_restart && c.status == doctor::Status::Ok {
                c.repair(Repair::RestartBrowser)
            } else {
                c
            }
        }),
    );
    let os = std::env::consts::OS;
    let dk = desktop.clone();
    add(
        "desktop_automation",
        "desktop automation",
        Box::new(move || doctor::check_desktop_automation(os, dk.as_ref())),
    );
    add(
        "capabilities",
        "capabilities",
        Box::new(move || doctor::check_capabilities(&capabilities(desktop.as_ref()))),
    );
    let d = data_dir.clone();
    add(
        "schedule_tools",
        "schedule tools",
        Box::new(move || doctor::check_schedule_tools(&d)),
    );
    let d = data_dir.clone();
    add(
        "process_guard",
        "process guard",
        Box::new(move || doctor::check_process_guard(&d)),
    );
    let safe = s.safe_mode();
    add(
        "safe_mode",
        "safe mode",
        Box::new(move || doctor::check_safe_mode(safe)),
    );
    let d = data_dir.clone();
    add(
        "disk_space",
        "disk space",
        Box::new(move || doctor::check_disk(&d)),
    );
    // The `local_models` line (RAM, GPU, pull command) is computed in-process by `bot doctor`
    // only: this route is reachable with the API token alone, `/v1/local-models` is owner-only.
    let d = data_dir;
    add(
        "clock",
        "clock",
        Box::new(move || doctor::check_clock(SystemTime::now(), doctor::newest_mtime(&d))),
    );
    run_jobs(jobs, CHECK_TIMEOUT, deadline, permit).await
}

/// Run the read-only diagnostics.
#[utoipa::path(get, path = "/v1/doctor", tag = "doctor", security(("bearer" = [])),
    responses((status = 200, body = DoctorReport), (status = 401, body = ErrorBody),
              (status = 429, body = ErrorBody, description = "a diagnostics run is already in progress")))]
pub(crate) async fn get_doctor(State(s): State<AppState>) -> Result<Json<DoctorReport>, ApiError> {
    let permit = Arc::new(s.doctor_permit()?);
    let checks = run_checks(&s, Instant::now() + OVERALL_TIMEOUT, &permit).await;
    Ok(Json(DoctorReport {
        ok: checks.iter().all(|c| c.status != doctor::Status::Fail),
        checks,
    }))
}

/// Run one allow-listed repair.  The request is the local user's click in
/// Settings (bots have no API token; paired peers are refused by the peer
/// router).  An attributed `doctor.repair_requested` audit record is written
/// before anything runs (no record, no repair), and `doctor.repair` records
/// the outcome.
#[utoipa::path(post, path = "/v1/doctor/repair/{id}", tag = "doctor", security(("bearer" = [])),
    params(("id" = String, Path, description = "Repair id: create_data_dir, reload_providers or restart_browser")),
    responses((status = 200, body = RepairResult), (status = 401, body = ErrorBody),
              (status = 403, body = ErrorBody, description = "not the local user"),
              (status = 404, body = ErrorBody, description = "not an allow-listed repair"),
              (status = 409, body = ErrorBody, description = "this daemon cannot run that repair"),
              (status = 422, body = ErrorBody, description = "the repair ran and failed"),
              (status = 500, body = ErrorBody, description = "the audit record could not be written; nothing ran")))]
pub(crate) async fn repair(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Result<Json<RepairResult>, ApiError> {
    let Some(repair) = Repair::parse(&id) else {
        return Err(ApiError::not_found(format!("repair {id}")));
    };
    // Defence in depth: the peer router already refuses `/v1/doctor*`.
    if !actor.local {
        return Err(ApiError::forbidden(
            "repairs are run by the local user from this computer's Settings",
        ));
    }
    let who = actor.key;
    s.storage()
        .append_audit(
            s.user_id(),
            None,
            &who,
            "doctor.repair_requested",
            &serde_json::json!({ "repair": repair.id() }),
        )
        .map_err(ApiError::internal)?;
    // Run detached from the request: a client that hangs up mid-repair must
    // not skip the outcome record.
    let app = s.clone();
    let task = tokio::spawn(async move {
        let outcome = run_repair(&app, repair).await;
        let (ok, detail) = match &outcome {
            Ok(d) => (true, d.clone()),
            Err(e) => (false, e.to_string()),
        };
        let audit = app.storage().append_audit(
            app.user_id(),
            None,
            &who,
            "doctor.repair",
            &serde_json::json!({ "repair": repair.id(), "ok": ok, "detail": detail }),
        );
        (outcome, audit)
    });
    let (outcome, audit) = task.await.map_err(ApiError::internal)?;
    audit.map_err(ApiError::internal)?;
    outcome.map(|detail| Json(RepairResult { repair, detail }))
}

async fn run_repair(s: &AppState, repair: Repair) -> Result<String, ApiError> {
    match repair {
        Repair::CreateDataDir => {
            let dir = s.data_dir();
            tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dir))
                .await
                .map_err(ApiError::internal)?
                .map_err(|e| {
                    ApiError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "repair_failed",
                        e.to_string(),
                    )
                })?;
            Ok("data dir is present".into())
        }
        Repair::ReloadProviders => {
            if s.provider_host().is_none() {
                return Err(ApiError::conflict(
                    "this daemon was started without provider configuration",
                ));
            }
            let r = crate::routes::reload_from_host(s).await?;
            Ok(format!("{} provider(s) loaded", r.active.len()))
        }
        Repair::RestartBrowser => {
            let host = s
                .doctor_host()
                .filter(|h| h.can_restart_browser())
                .ok_or_else(|| ApiError::conflict("this daemon cannot restart the browser"))?;
            host.restart_browser()
                .await
                .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "repair_failed", e))
        }
    }
}

/// A redacted Markdown support report: versions, platform, check results and
/// config file names (never contents).  The home directory becomes `~`;
/// secrets known to the daemon (and their encoded forms), the API token,
/// key-shaped strings, credentials in URLs and assignments, host names, user
/// names and e-mail addresses are masked.  Ends within 15 s.
#[utoipa::path(get, path = "/v1/doctor/report", tag = "doctor", security(("bearer" = [])),
    responses((status = 200, description = "Markdown report", content_type = "text/markdown", body = String),
              (status = 401, body = ErrorBody), (status = 429, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "safe redaction unavailable or the report did not finish in time")))]
pub(crate) async fn report(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let permit = Arc::new(s.doctor_permit()?);
    let deadline = Instant::now() + REPORT_TIMEOUT;
    let text = build_report(&s, doctor::home_dir(), deadline, &permit).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        text,
    ))
}

/// The redacted report text, with `home` replaced by `~`, ending by
/// `deadline`.
pub(crate) async fn build_report(
    s: &AppState,
    home: Option<String>,
    deadline: Instant,
    permit: &RunPermit,
) -> Result<String, ApiError> {
    let checks = run_checks(s, deadline.min(Instant::now() + OVERALL_TIMEOUT), permit).await;
    let dir = s.data_dir();
    let production = s.doctor_host().and_then(|h| h.redactor());
    let store = s.secret_store();
    let token = s.token().to_owned();
    let app = s.clone();
    let hold = Arc::clone(permit);
    let task = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        let redactor = if let Some(r) = production {
            r
        } else {
            // No live redactor from the embedder: build one from the same
            // store, with encoded variants.  Fail closed when the store
            // cannot be read.
            let r = agent_core::SharedRedactor::default();
            r.refresh_from_store(store.as_deref(), &[token])
                .map_err(|e| {
                    ApiError::unavailable(format!(
                        "secret redaction is unavailable ({e}); no report was produced"
                    ))
                })?;
            r
        };
        let redact = |t: &str| redactor.redact(t);
        let scrub = doctor::Scrub {
            secrets: Some(&redact),
            home: home.as_deref(),
            identities: doctor::machine_identities(),
            known: doctor::KnownIds::collect(&dir, app.provider_host().map(|h| &h.catalog)),
        };
        let info = doctor::ReportInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            api_version: crate::API_VERSION.into(),
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            config_files: doctor::present_config_files(&dir),
            data_dir: dir,
        };
        let markdown = doctor::report_markdown(&info, &checks, &scrub);
        // Once more over the whole text (secrets, encodings, token shapes);
        // every value was fully scrubbed already, and the Markdown table and
        // fixed template must stay intact.
        let text = doctor::redact_report_text(&markdown, &scrub);
        if redactor.contains_secret(&text) {
            return Err(ApiError::unavailable(
                "the report still matched a known secret after redaction; it was withheld",
            ));
        }
        Ok(text)
    });
    match tokio::time::timeout_at(deadline, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(ApiError::internal(e)),
        Err(_) => Err(ApiError::unavailable(
            "the support report did not finish in time; try again in a moment",
        )),
    }
}
