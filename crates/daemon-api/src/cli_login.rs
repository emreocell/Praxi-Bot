//! "Sign in on the server": the owner's paired desktop signs a coding CLI in ON THE HOST.
//!
//! Same authority as the engine install route (`host_admin`): a paired desktop whose
//! `Device.admin` is read from the pairing store on every request, host mode only; the client
//! names an engine id and nothing else. One login runs at a time (a one-slot semaphore).
//!
//! The login itself is the vendor's own command from the fixed registry in
//! `cli_provider::login`. This module is only the job state machine around it:
//!
//! ```text
//! starting -> needs_browser {url, code}   (device-code flow: open the page, type the code;
//!                                          browser flow: open the page, nothing to type)
//!          -> needs_code {url}            (paste-code flow: sign in, paste the page's code back)
//!          -> verifying -> done           (the host's own detection reports "signed in")
//!                       -> failed {code} | timed_out | cancelled
//! ```
//!
//! Nothing secret is ever kept or sent: the output stays private to `LoginRun`, a pasted code
//! goes to the process' stdin and is dropped, and the audit rows name the engine and the
//! outcome only. The whole process tree is killed on cancel, timeout or shutdown.
//!
//! Engines whose CLI only talks on a terminal run on a pseudo terminal (`cli_provider::login`,
//! `pty_session`); the state machine is the same. "Signed in" is confirmed by the CLI's own
//! status where it has one, else by the success line it printed together with a clean exit, or
//! (a CLI that keeps running) by the credential file it wrote after the code was handed over.
//! The account name the CLI reports (never a token) is kept for the app to show.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use cli_provider::login::{self, FlowKind, LoginSpec, Prompt};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

use crate::{
    ApiError, AppState, PeerIdentity, PeerRole,
    host_admin::{Gate, audit, require_admin},
    routes::ApiJson,
};

/// The whole login, from start to a confirmed sign-in.
pub(crate) const LOGIN_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// How long the CLI may take to show its page.
pub(crate) const STARTUP: Duration = Duration::from_secs(30);

/// Tests can shorten the login lifetime (milliseconds; 0 = production value).
#[cfg(test)]
pub(crate) static LIFETIME_TEST_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn lifetime() -> Duration {
    #[cfg(test)]
    if let ms @ 1.. = LIFETIME_TEST_MS.load(std::sync::atomic::Ordering::SeqCst) {
        return Duration::from_millis(ms);
    }
    LOGIN_LIFETIME
}

/// How long the CLI may take after the code was handed to it.
const AFTER_CODE: Duration = Duration::from_secs(60);
/// How often the credential files are looked at.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// How often the owner desktop's admin right is read again while a sign-in runs.
const ADMIN_RECHECK: Duration = Duration::from_secs(2);

/// Completes once the desktop that started the sign-in is no longer a host administrator
/// (the pairing store is read on every look, as for a terminal session).
async fn revoked(still_admin: &impl Fn() -> bool) {
    let mut every =
        tokio::time::interval_at(tokio::time::Instant::now() + ADMIN_RECHECK, ADMIN_RECHECK);
    loop {
        every.tick().await;
        if !still_admin() {
            return;
        }
    }
}

#[derive(Debug, Clone)]
struct Job {
    id: u64,
    state: &'static str,
    url: Option<String>,
    code: Option<String>,
    error: Option<&'static str>,
    cancel: CancellationToken,
    /// Unpredictable id returned to the initiating desktop; every later call must present it.
    token: String,
    /// The paired desktop (peer key) that started this sign-in; nobody else may touch it.
    owner: String,
    code_tx: Option<tokio::sync::mpsc::Sender<zeroize::Zeroizing<String>>>,
    /// The account the CLI reported once it signed in (an e-mail or a user name).
    account: Option<String>,
}

fn new_token() -> Result<String, ApiError> {
    use base64::Engine as _;
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::internal("no randomness"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The job `token` of `engine` as seen by `device`, or why it may not be touched. The caller
/// holds the book's lock, so the check and whatever follows are one atomic step.
fn owned_job<'a>(
    jobs: &'a mut HashMap<String, Job>,
    engine: &str,
    device: &str,
    token: &str,
) -> Result<&'a mut Job, ApiError> {
    let stale = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "login_stale",
            "this sign-in is no longer the current one; start it again",
        )
    };
    let job = jobs.get_mut(engine).ok_or_else(stale)?;
    if job.owner != device {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "login_forbidden",
            "this sign-in belongs to another desktop",
        ));
    }
    if job.token != token {
        return Err(stale());
    }
    Ok(job)
}

/// `?job=` of the status and cancel calls.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct JobQuery {
    /// The `job_id` the sign-in was started with.
    pub job: Option<String>,
}

pub(crate) struct LoginBook {
    pub(crate) running: Arc<tokio::sync::Semaphore>,
    jobs: parking_lot::Mutex<HashMap<String, Job>>,
    next: std::sync::atomic::AtomicU64,
}

impl Default for LoginBook {
    fn default() -> Self {
        Self {
            running: Arc::new(tokio::sync::Semaphore::new(1)),
            jobs: parking_lot::Mutex::new(HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

/// `GET/POST/DELETE /v1/admin/cli-engines/{id}/login`: where the sign-in stands.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminCliLoginDto {
    pub engine: String,
    /// `idle`, `starting`, `needs_browser`, `needs_code`, `verifying`, `done`, `failed`,
    /// `timed_out` or `cancelled`.
    pub state: String,
    /// How this engine can be signed in from the desktop: `device_code`, `paste_code`,
    /// `browser` (open the link, nothing to type) or `manual` (it cannot be driven remotely:
    /// run `command` over SSH).
    pub flow: String,
    /// The vendor's sign-in page (verified against its allow-listed hosts).
    pub url: Option<String>,
    /// The one-time code to type on that page (device-code flow).
    pub code: Option<String>,
    /// Stable code of the failure (`login_start_timeout`, `login_code_rejected`, ...).
    pub error: Option<String>,
    /// The exact command to run over SSH for a `manual` engine.
    pub command: Option<String>,
    /// Found on the host right now.
    pub installed: bool,
    /// Signed in, when the CLI can say.
    pub signed_in: Option<bool>,
    /// The account this engine is signed in as (an e-mail or user name), when the CLI names
    /// one. Never a token or key.
    pub account: Option<String>,
    /// Unpredictable id of this sign-in; status, code and cancel must present it (and come from
    /// the desktop that started it).
    pub job_id: Option<String>,
}

/// `POST .../login/code`: the code the vendor's page showed.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminLoginCodeInput {
    /// The sign-in this code is for (`job_id` of the start answer).
    pub job_id: String,
    /// One line, 8-1024 plain characters. Never logged, echoed or stored; zeroised after use.
    pub code: String,
}

impl Drop for AdminLoginCodeInput {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.code);
    }
}

impl std::fmt::Debug for AdminLoginCodeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminLoginCodeInput")
            .field("code", &"<redacted>")
            .finish()
    }
}

fn spec_of(engine: &str) -> Result<&'static cli_provider::registry::CliEngineSpec, ApiError> {
    cli_provider::engine_spec(engine).ok_or_else(|| ApiError::not_found("unknown engine"))
}

pub(crate) fn flow_name(engine: &str) -> &'static str {
    match login::login_spec(engine).map(|s| s.kind) {
        Some(FlowKind::DeviceCode) => "device_code",
        Some(FlowKind::PasteCode) => "paste_code",
        Some(FlowKind::Browser) => "browser",
        None => "manual",
    }
}

async fn dto(
    s: &AppState,
    engine: &'static cli_provider::registry::CliEngineSpec,
    job: Option<Job>,
) -> AdminCliLoginDto {
    let row = s
        .detector()
        .detect(false)
        .await
        .into_iter()
        .find(|r| r.id == engine.id);
    let flow = flow_name(engine.id);
    AdminCliLoginDto {
        engine: engine.id.into(),
        state: job.as_ref().map_or("idle", |j| j.state).into(),
        flow: flow.into(),
        url: job.as_ref().and_then(|j| j.url.clone()),
        code: job.as_ref().and_then(|j| j.code.clone()),
        error: job.as_ref().and_then(|j| j.error).map(str::to_owned),
        command: (flow == "manual").then(|| engine.login.to_owned()),
        job_id: job.as_ref().map(|j| j.token.clone()),
        installed: row.as_ref().is_some_and(|r| r.installed),
        account: job
            .as_ref()
            .and_then(|j| j.account.clone())
            .or_else(|| row.as_ref().and_then(|r| r.account_label.clone())),
        signed_in: row.and_then(|r| match r.auth {
            cli_provider::AuthState::SignedIn => Some(true),
            cli_provider::AuthState::SignedOut => Some(false),
            cli_provider::AuthState::Unknown => None,
        }),
    }
}

fn update(s: &AppState, engine: &str, id: u64, f: impl FnOnce(&mut Job)) {
    if let Some(job) = s.0.cli_logins.jobs.lock().get_mut(engine)
        && job.id == id
    {
        f(job);
    }
}

fn finish(s: &AppState, engine: &str, id: u64, state: &'static str, error: Option<&'static str>) {
    update(s, engine, id, |j| {
        j.state = state;
        j.error = error;
        j.code_tx = None;
    });
}

/// A stable reason for a login that ended without a sign-in, read from the CLI's private output.
fn classify(output: &str, exit_ok: bool) -> &'static str {
    let lower = output.to_ascii_lowercase();
    if lower.contains("device-auth")
        && (lower.contains("unexpected argument")
            || lower.contains("unrecognized")
            || lower.contains("unknown option"))
    {
        "login_cli_outdated"
    } else if lower.contains("device")
        && (lower.contains("disabled")
            || lower.contains("not enabled")
            || lower.contains("not allowed"))
    {
        "device_auth_disabled"
    } else if lower.contains("expired") || lower.contains("timed out") {
        "login_expired"
    } else if !exit_ok
        && [
            "code rejected",
            "code was rejected",
            "invalid authorization code",
            "invalid device code",
            "did not accept",
            "incorrect code",
        ]
        .iter()
        .any(|phrase| lower.contains(*phrase))
    {
        "login_code_rejected"
    } else {
        "login_failed"
    }
}

/// Run the whole login. The permit is held until the process tree is gone.
#[allow(clippy::too_many_lines)] // one ordered state machine: start, prompt, code, exit, verify
async fn run_job(
    s: AppState,
    spec: &'static LoginSpec,
    id: u64,
    owner: String,
    cancel: CancellationToken,
    mut code_rx: tokio::sync::mpsc::Receiver<zeroize::Zeroizing<String>>,
) {
    let engine = spec.engine;
    let shutdown = s.shutdown_token();
    let still_admin = || {
        s.is_host_admin(Some(&PeerIdentity {
            key: owner.clone(),
            role: PeerRole::Desktop,
        }))
    };
    let deadline = tokio::time::Instant::now() + lifetime();
    let detector = s.detector();
    let pins = read_pins(&s.data_dir()).remove(engine);
    let mut run = match login::start(spec, detector.environment(), pins.as_deref()) {
        Ok(run) => run,
        Err(login::StartError::NotInstalled) => {
            finish(&s, engine, id, "failed", Some("not_installed"));
            return;
        }
        Err(login::StartError::SpawnFailed) => {
            finish(&s, engine, id, "failed", Some("spawn_failed"));
            return;
        }
        Err(login::StartError::FilesChanged) => {
            finish(&s, engine, id, "failed", Some("login_files_changed"));
            return;
        }
        Err(login::StartError::UntrustedLocation) => {
            finish(&s, engine, id, "failed", Some("login_untrusted_location"));
            return;
        }
    };
    // 1. The page (and code) the vendor's CLI wants the person to use.
    let prompt = tokio::select! {
        p = run.next_prompt(spec, STARTUP) => p,
        () = cancel.cancelled() => { run.terminate().await; finish(&s, engine, id, "cancelled", None); return; }
        () = shutdown.cancelled() => { run.terminate().await; finish(&s, engine, id, "cancelled", None); return; }
        () = revoked(&still_admin) => { run.terminate().await; finish(&s, engine, id, "failed", Some("admin_revoked")); return; }
    };
    let Some(Prompt::Page { url, code }) = prompt else {
        let reason = classify(&run.private_output(), false);
        run.terminate().await;
        finish(
            &s,
            engine,
            id,
            "failed",
            Some(if reason == "login_failed" {
                "login_start_timeout"
            } else {
                reason
            }),
        );
        return;
    };
    let paste = spec.kind == FlowKind::PasteCode;
    // Whatever the CLI wrote while it started up is not the sign-in: only a change from here on.
    run.watch_credentials(spec, detector.environment());
    update(&s, engine, id, |j| {
        j.state = if paste { "needs_code" } else { "needs_browser" };
        j.url = Some(url);
        j.code = code;
    });
    // 2. A pasted code (paste flow), then the exit (or the credential file of a CLI that keeps
    //    running) and the confirmation.
    let mut sent_at: Option<tokio::time::Instant> = None;
    // The first look comes after a full period, never at once.
    let first = tokio::time::Instant::now() + WATCH_EVERY;
    let mut watch = tokio::time::interval_at(first, WATCH_EVERY);
    let mut recheck = tokio::time::interval_at(first, ADMIN_RECHECK);
    let ended = loop {
        let limit = sent_at.map_or(deadline, |t| (t + AFTER_CODE).min(deadline));
        tokio::select! {
            biased;
            () = cancel.cancelled() => { run.terminate().await; finish(&s, engine, id, "cancelled", None); return; }
            () = shutdown.cancelled() => { run.terminate().await; finish(&s, engine, id, "cancelled", None); return; }
            // The owner's admin right can be withdrawn while the sign-in waits: stop at once, and
            // do not hand a pasted code to the process after that.
            _ = recheck.tick() => {
                if !still_admin() {
                    run.terminate().await;
                    finish(&s, engine, id, "failed", Some("admin_revoked"));
                    return;
                }
            }
            () = tokio::time::sleep_until(limit) => {
                run.terminate().await;
                if sent_at.is_some() && tokio::time::Instant::now() < deadline {
                    finish(&s, engine, id, "failed", Some("login_not_confirmed"));
                } else {
                    finish(&s, engine, id, "timed_out", Some("login_timed_out"));
                }
                return;
            }
            Some(pasted) = code_rx.recv(), if paste && sent_at.is_none() => {
                if !still_admin() {
                    run.terminate().await;
                    finish(&s, engine, id, "failed", Some("admin_revoked"));
                    return;
                }
                // Straight to the process' stdin; the value is dropped right after.
                if run.send_line(pasted.as_str()).await.is_err() {
                    run.terminate().await;
                    finish(&s, engine, id, "failed", Some("login_failed"));
                    return;
                }
                drop(pasted);
                sent_at = Some(tokio::time::Instant::now());
                update(&s, engine, id, |j| j.state = "verifying");
            }
            _ = watch.tick(), if !spec.credentials.is_empty() && (sent_at.is_some() || !paste) => {
                if run.credential_written(spec, detector.environment()) {
                    // The CLI keeps running after it signed in: give it a moment to finish
                    // writing, then stop it.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    break Ended::Credential;
                }
            }
            exit = run.wait_exit(Duration::from_secs(3600)) => break Ended::Exit(matches!(exit, Some(Some(0)))),
        }
    };
    // 3. Confirm: with the CLI's own status where it has one, the way turns will see it.
    let output = run.private_output();
    let wrote = run.credential_written(spec, detector.environment());
    if ended == Ended::Exit(false) {
        let reason = classify(&output, false);
        run.terminate().await;
        finish(&s, engine, id, "failed", Some(reason));
        return;
    }
    run.terminate().await;
    update(&s, engine, id, |j| j.state = "verifying");
    let row = s
        .detector()
        .detect(true)
        .await
        .into_iter()
        .find(|r| r.id == engine);
    match confirm(spec, ended, wrote, row.as_ref().map(|r| r.auth), &output) {
        Confirmed::Yes => {
            let account =
                login::parse_account(&output).or_else(|| row.and_then(|r| r.account_label));
            update(&s, engine, id, |j| j.account = account);
            finish(&s, engine, id, "done", None);
        }
        Confirmed::No => finish(&s, engine, id, "failed", Some("login_not_confirmed")),
    }
}

/// How the CLI stopped (or was stopped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The process ended; `true` when it exited with code 0.
    Exit(bool),
    /// The CLI keeps running; its credential file was written.
    Credential,
}

#[derive(Debug, PartialEq, Eq)]
enum Confirmed {
    Yes,
    No,
}

/// Was the sign-in successful? The CLI's own status decides when it has one (signed in: yes,
/// signed out: no). A CLI without a status command is confirmed by its success line after a clean
/// exit, or by the credential file it wrote during this sign-in (`wrote`). Never from an exit
/// code alone.
fn confirm(
    spec: &LoginSpec,
    ended: Ended,
    wrote: bool,
    auth: Option<cli_provider::AuthState>,
    output: &str,
) -> Confirmed {
    use cli_provider::AuthState;
    match auth {
        Some(AuthState::SignedIn) => Confirmed::Yes,
        Some(AuthState::SignedOut) => Confirmed::No,
        Some(AuthState::Unknown) | None => match ended {
            Ended::Credential => Confirmed::Yes,
            Ended::Exit(_) if wrote || login::printed_success(spec, output) => Confirmed::Yes,
            Ended::Exit(_) => Confirmed::No,
        },
    }
}

// ------------------------------------------------------------------ install pins

const PINS_FILE: &str = "cli-install-pins.json";

/// What the install flow recorded of each engine's entry point (real path, size, SHA-256).
/// The file lives in the daemon's data directory, which the bots' file tools cannot write.
pub(crate) fn read_pins(data: &std::path::Path) -> HashMap<String, Vec<login::FilePin>> {
    std::fs::read(data.join(PINS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_pins(
    data: &std::path::Path,
    pins: &HashMap<String, Vec<login::FilePin>>,
) -> std::io::Result<()> {
    use std::io::Write;
    let path = data.join(PINS_FILE);
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec(pins)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)
}

/// Called by the install flow right after a successful install: pin the engine's files as they
/// are now. A later sign-in refuses to run anything else.
pub(crate) async fn record_install_pin(s: &AppState, engine: &'static str) {
    let env = s.detector().environment().clone();
    let data = s.data_dir();
    let _ = tokio::task::spawn_blocking(move || record_cli_install_pin(&data, &env, engine)).await;
}

/// The same pin for an install that did not go through the daemon (`bot cli install`, run as the
/// service user in a terminal): without it the app would refuse to sign in to a CLI that Praxi
/// itself installed, as it does for one nobody can vouch for.
///
/// The pins decide which programs a later sign-in may run, so they are only written by the user
/// that owns the data folder (the service user): returns `false`, and writes nothing, when that
/// folder belongs to somebody else.
#[must_use]
pub fn record_cli_install_pin(
    data: &std::path::Path,
    env: &cli_provider::PathEnv,
    engine: &str,
) -> bool {
    if !data_dir_is_ours(data) {
        return false;
    }
    let mut pins = read_pins(data);
    match login::pin_installed(engine, env) {
        Ok(files) => {
            pins.insert(engine.to_owned(), files);
        }
        Err(_) => {
            pins.remove(engine);
        }
    }
    write_pins(data, &pins).is_ok()
}

/// The data folder does not exist yet (this user creates it) or belongs to the effective user.
#[cfg(unix)]
fn data_dir_is_ours(data: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(data).map_or_else(
        |e| e.kind() == std::io::ErrorKind::NotFound,
        |m| m.uid() == rustix::process::geteuid().as_raw(),
    )
}

#[cfg(not(unix))]
fn data_dir_is_ours(_data: &std::path::Path) -> bool {
    true
}

fn trust_error(e: login::TrustError) -> ApiError {
    match e {
        login::TrustError::FilesChanged => ApiError::new(
            StatusCode::CONFLICT,
            "login_files_changed",
            "the engine's files changed since installation: reinstall it (Install on server) before signing in",
        ),
        login::TrustError::UntrustedLocation => ApiError::new(
            StatusCode::CONFLICT,
            "login_untrusted_location",
            "this engine was not installed by Praxi and is not in a system location: reinstall it with Install on server, or sign in as the user Praxi runs as (in the app's terminal, or over SSH with `sudo praxi cli login <name>`)",
        ),
        login::TrustError::NotInstalled => ApiError::new(
            StatusCode::CONFLICT,
            "not_installed",
            "install this engine on the server first",
        ),
        login::TrustError::Unreadable => ApiError::new(
            StatusCode::CONFLICT,
            "login_files_changed",
            "the engine's files could not be read: reinstall it before signing in",
        ),
    }
}

// ------------------------------------------------------------------ routes

/// Start the engine's own sign-in ON THE HOST. Admin desktop only; one at a time; the sign-in
/// belongs to the desktop that started it.
#[utoipa::path(post, operation_id = "admin_cli_engine_login", path = "/v1/admin/cli-engines/{id}/login", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)),
    responses((status = 202, body = AdminCliLoginDto), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
#[allow(clippy::too_many_lines)] // one admission sequence: engine, trust, slot, audit, spawn
pub(crate) async fn start_login(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<AdminCliLoginDto>), ApiError> {
    let admin = require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let device = peer.as_ref().map(|p| p.0.key.clone()).unwrap_or_default();
    let engine = spec_of(&id)?;
    let Some(spec) = login::login_spec(engine.id) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "login_manual",
            format!(
                "this engine cannot be signed in from here: run `{}` on the server",
                engine.login
            ),
        ));
    };
    let row = s
        .detector()
        .detect(false)
        .await
        .into_iter()
        .find(|r| r.id == engine.id);
    if !row.as_ref().is_some_and(|r| r.installed) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_installed",
            "install this engine on the server first",
        ));
    }
    let token = new_token()?;
    // A working sign-in is never replaced from here.
    if row
        .as_ref()
        .is_some_and(|r| r.auth == cli_provider::AuthState::SignedIn)
    {
        let job = Job {
            id: s
                .0
                .cli_logins
                .next
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            state: "done",
            url: None,
            code: None,
            error: None,
            cancel: CancellationToken::new(),
            token,
            owner: device,
            code_tx: None,
            account: row.as_ref().and_then(|r| r.account_label.clone()),
        };
        s.0.cli_logins
            .jobs
            .lock()
            .insert(engine.id.to_owned(), job.clone());
        return Ok((StatusCode::ACCEPTED, Json(dto(&s, engine, Some(job)).await)));
    }
    // The executable must be the one the install flow pinned (or a system install).
    {
        let env = s.detector().environment().clone();
        let pinned = read_pins(&s.data_dir()).remove(engine.id);
        let name = engine.id;
        tokio::task::spawn_blocking(move || login::verify_engine(name, &env, pinned.as_deref()))
            .await
            .map_err(ApiError::internal)?
            .map_err(trust_error)?;
    }
    let permit = Arc::clone(&s.0.cli_logins.running)
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "login_busy",
                "a sign-in is already running on this server",
            )
        })?;
    audit(
        &s,
        &admin,
        "provider.cli_login.admin_started",
        &serde_json::json!({"engine": engine.id}),
    )?;
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let cancel = CancellationToken::new();
    let job_id =
        s.0.cli_logins
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let job = Job {
        id: job_id,
        state: "starting",
        url: None,
        code: None,
        error: None,
        cancel: cancel.clone(),
        token,
        owner: device.clone(),
        code_tx: Some(tx),
        account: None,
    };
    s.0.cli_logins
        .jobs
        .lock()
        .insert(engine.id.to_owned(), job.clone());
    let task = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        run_job(task.clone(), spec, job_id, device, cancel, rx).await;
        let outcome = task
            .0
            .cli_logins
            .jobs
            .lock()
            .get(spec.engine)
            .filter(|j| j.id == job_id)
            .map(|j| (j.state, j.error));
        let _ = task.detector().detect(true).await;
        let _ = audit(
            &task,
            &admin,
            "provider.cli_login.admin_finished",
            &serde_json::json!({"engine": spec.engine, "state": outcome.map(|o| o.0), "error": outcome.and_then(|o| o.1)}),
        );
    });
    Ok((StatusCode::ACCEPTED, Json(dto(&s, engine, Some(job)).await)))
}

/// Where the sign-in above stands (poll while it is `starting`, `needs_*` or `verifying`).
/// Needs the `job` id and the desktop that started it.
#[utoipa::path(get, operation_id = "admin_cli_engine_login_status", path = "/v1/admin/cli-engines/{id}/login", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path), JobQuery),
    responses((status = 200, body = AdminCliLoginDto), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn login_status(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<JobQuery>,
) -> Result<Json<AdminCliLoginDto>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let device = peer.as_ref().map(|p| p.0.key.clone()).unwrap_or_default();
    let engine = spec_of(&id)?;
    let job = {
        let mut jobs = s.0.cli_logins.jobs.lock();
        if jobs.contains_key(engine.id) {
            Some(
                owned_job(
                    &mut jobs,
                    engine.id,
                    &device,
                    query.job.as_deref().unwrap_or_default(),
                )?
                .clone(),
            )
        } else {
            None
        }
    };
    Ok(Json(dto(&s, engine, job).await))
}

/// Hand the code the vendor's page showed to the waiting CLI. Exactly one code per sign-in,
/// and only from the desktop that started it, naming that very sign-in.
#[utoipa::path(post, operation_id = "admin_cli_engine_login_code", path = "/v1/admin/cli-engines/{id}/login/code", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path)), request_body = AdminLoginCodeInput,
    responses((status = 202, body = AdminCliLoginDto), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn login_code(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<AdminLoginCodeInput>,
) -> Result<(StatusCode, Json<AdminCliLoginDto>), ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let device = peer.as_ref().map(|p| p.0.key.clone()).unwrap_or_default();
    let engine = spec_of(&id)?;
    let code = zeroize::Zeroizing::new(
        login::valid_code(&input.code)
            .ok_or_else(|| {
                ApiError::bad_request("paste the whole code shown on the vendor's page")
            })?
            .to_owned(),
    );
    // One atomic step: this device, this very sign-in, waiting for a code, sender not yet taken.
    let (tx, job) = {
        let mut jobs = s.0.cli_logins.jobs.lock();
        let job = owned_job(&mut jobs, engine.id, &device, &input.job_id)?;
        if job.state != "needs_code" {
            return Err(ApiError::conflict("no sign-in is waiting for a code"));
        }
        let tx = job
            .code_tx
            .take()
            .ok_or_else(|| ApiError::conflict("a code was already sent for this sign-in"))?;
        (tx, job.clone())
    };
    tx.send(code)
        .await
        .map_err(|_| ApiError::conflict("the sign-in is no longer running"))?;
    Ok((StatusCode::ACCEPTED, Json(dto(&s, engine, Some(job)).await)))
}

/// Stop the sign-in (the whole process tree is killed). Needs the `job` id and the desktop that
/// started it: a stale window cannot cancel a newer sign-in.
#[utoipa::path(delete, operation_id = "admin_cli_engine_login_cancel", path = "/v1/admin/cli-engines/{id}/login", tag = "admin", security(("bearer" = [])),
    params(("id" = String, Path), JobQuery),
    responses((status = 200, body = AdminCliLoginDto), (status = 403, body = crate::error::ErrorBody), (status = 409, body = crate::error::ErrorBody)))]
pub(crate) async fn cancel_login(
    State(s): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    _gate: Gate,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<JobQuery>,
) -> Result<Json<AdminCliLoginDto>, ApiError> {
    require_admin(&s, peer.as_ref().map(|p| &p.0))?;
    let device = peer.as_ref().map(|p| p.0.key.clone()).unwrap_or_default();
    let engine = spec_of(&id)?;
    let token = query.job.unwrap_or_default();
    owned_job(&mut s.0.cli_logins.jobs.lock(), engine.id, &device, &token)?
        .cancel
        .cancel();
    // Give the task a moment to kill the tree and record the state.
    for _ in 0..50 {
        let state = s.0.cli_logins.jobs.lock().get(engine.id).map(|j| j.state);
        if !matches!(
            state,
            Some("starting" | "needs_browser" | "needs_code" | "verifying")
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let job = owned_job(&mut s.0.cli_logins.jobs.lock(), engine.id, &device, &token)
        .ok()
        .map(|j| j.clone());
    Ok(Json(dto(&s, engine, job).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CLI without a status command is confirmed by a success line after a clean exit; a
    /// denial that contains the same words ("not logged in") is not one.
    #[test]
    fn a_negated_success_phrase_does_not_confirm_a_sign_in() {
        let kiro = login::login_spec("kiro").unwrap();
        let ended = Ended::Exit(true);
        for denial in [
            "You are not logged in\n",
            "Error: not logged in. Run `kiro-cli login` again\n",
            "Session expired, you are no longer logged in\n",
        ] {
            assert_eq!(
                confirm(kiro, ended, false, None, denial),
                Confirmed::No,
                "{denial:?}"
            );
        }
        assert_eq!(
            confirm(kiro, ended, false, None, "Logged in successfully\n"),
            Confirmed::Yes
        );
        // The CLI's own status still decides first.
        assert_eq!(
            confirm(
                kiro,
                ended,
                false,
                Some(cli_provider::AuthState::SignedOut),
                "Logged in\n"
            ),
            Confirmed::No
        );
    }

    /// An install made in a terminal (`bot cli install`, as the service user) is pinned just like
    /// the app's own, so the app signs in to it; a vanished install drops its pin.
    #[cfg(unix)]
    #[test]
    fn a_terminal_install_is_pinned_for_the_apps_sign_in() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\necho 2.1.300\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = cli_provider::PathEnv::for_test(bin);

        assert!(record_cli_install_pin(&data, &env, "claude"));
        let pins = read_pins(&data);
        let files = pins.get("claude").expect("pinned");
        assert!(
            files.iter().all(|f| f.size > 0 && f.sha256.len() == 64),
            "{files:?}"
        );
        // The pin is what lets the sign-in run a program outside the system folders.
        assert!(login::verify_engine("claude", &env, Some(files)).is_ok());
        assert!(login::verify_engine("claude", &env, None).is_err());

        std::fs::remove_file(&claude).unwrap();
        assert!(record_cli_install_pin(&data, &env, "claude"));
        assert!(!read_pins(&data).contains_key("claude"));
    }

    /// The pins say which programs the service may later run: a user who does not own the data
    /// folder (an SSH user running `bot cli install` by mistake) must not be able to write them.
    #[test]
    fn login_code_rejection_requires_a_failed_exit_or_an_explicit_message() {
        assert_eq!(
            classify("Enter code: ABCD\nWaiting for auth...\n", true),
            "login_failed"
        );
        assert_eq!(
            classify("invalid authorization code\n", false),
            "login_code_rejected"
        );
        assert_eq!(
            classify("The code was rejected\n", false),
            "login_code_rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_install_by_another_user_records_no_pin() {
        let dir = tempfile::tempdir().unwrap();
        let env = cli_provider::PathEnv::for_test(dir.path().join("bin"));
        // `/usr` belongs to root. (Running the tests as root would own it: nothing to prove then.)
        let other = std::path::Path::new("/usr");
        if rustix::process::geteuid().as_raw() != 0 {
            assert!(!record_cli_install_pin(other, &env, "claude"));
            assert!(!other.join(PINS_FILE).exists());
        }
        // A data folder that is not there yet is created by the user who installs.
        assert!(data_dir_is_ours(&dir.path().join("fresh")));
        assert!(data_dir_is_ours(dir.path()));
    }
}
