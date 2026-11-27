//! `/v1/doctor*`: auth, read-only checks, allow-listed repairs, redaction.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use secret_store::SecretStore;

use super::{T, call, call_token_only, send, setup};
use crate::doctor::{Check, Status};
use crate::doctor_routes::{DoctorHost, Job, RunPermit, on_blocking_pool, run_jobs};

fn permit(t: &T) -> RunPermit {
    Arc::new(t.state.doctor_permit().expect("diagnostics slot free"))
}

fn free_permit() -> RunPermit {
    Arc::new(
        Arc::new(tokio::sync::Semaphore::new(1))
            .try_acquire_owned()
            .unwrap(),
    )
}

fn audit_actions(t: &T) -> Vec<(String, serde_json::Value)> {
    t.state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "doctor.repair")
        .map(|a| (a.action, a.detail))
        .collect()
}

/// `(action, actor, repair)` of every doctor audit record, oldest first.
fn audit_trail(t: &T) -> Vec<(String, String, String)> {
    let mut v: Vec<_> = t
        .state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .into_iter()
        .filter(|a| a.action.starts_with("doctor."))
        .collect();
    v.sort_by_key(|a| a.id);
    v.into_iter()
        .map(|a| {
            (
                a.action,
                a.actor,
                a.detail["repair"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn every_doctor_route_needs_the_token() {
    let t = setup();
    for (m, p) in [
        (Method::GET, "/v1/doctor"),
        (Method::GET, "/v1/doctor/report"),
        (Method::POST, "/v1/doctor/repair/create_data_dir"),
    ] {
        let r = send(&t.app, m.clone(), p, None, &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        let r = send(
            &t.app,
            m.clone(),
            p,
            None,
            &[("authorization", "Bearer wrong")],
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
    }
    assert_eq!(audit_actions(&t), []);
}

#[tokio::test]
async fn doctor_lists_typed_checks_without_side_effects() {
    let t = setup();
    let dir = t.state.data_dir();
    let before: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let r = call(&t, Method::GET, "/v1/doctor", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let v = r.json();
    let checks = v["checks"].as_array().unwrap();
    let ids: Vec<&str> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
    for want in [
        "daemon",
        "host_ownership",
        "peer_link",
        "data_dir",
        "unicode_path",
        "config",
        "providers",
        "providers_loaded",
        "cli_accounts",
        "node_runtime",
        "browser",
        "desktop_automation",
        "capabilities",
        "schedule_tools",
        "full_access_lane",
        "disk_space",
        "clock",
    ] {
        assert!(ids.contains(&want), "{want} missing from {ids:?}");
    }
    let get = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap();
    assert_eq!(get("data_dir")["status"], "ok");
    assert_eq!(get("full_access_lane")["status"], "ok");
    assert_eq!(
        get("unicode_path")["status"],
        "ok",
        "{}",
        get("unicode_path")
    );
    // Sealed PATH in the test setup: no coding CLI installed, which is neutral.
    assert_eq!(get("cli_accounts")["status"], "skipped");
    // The sealed PATH has no Node.js either; the test machine is not an installer-managed server, so
    // that is neutral too (and the row names what it looked for).
    assert_eq!(get("node_runtime")["title"], "node / npm");
    assert!(
        get("node_runtime")["detail"]
            .as_str()
            .unwrap()
            .contains("node: not found; npm: not found"),
        "{}",
        get("node_runtime")
    );
    assert_eq!(get("peer_link")["status"], "skipped");
    // No embedder host: no browser restart offered (a computer with no browser at all is
    // offered the install instead, which depends on the machine running the test).
    assert_ne!(get("browser")["repair"], "restart_browser");
    assert!(get("browser")["repair"].is_null() || get("browser")["repair"] == "install_browser");
    assert_ne!(get("desktop_automation")["status"], "fail");
    for c in checks {
        assert!(c["title"].is_string() && c["detail"].is_string(), "{c}");
        assert!(
            ["ok", "warn", "fail", "skipped"].contains(&c["status"].as_str().unwrap()),
            "{c}"
        );
    }
    let after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        before.len(),
        after.len(),
        "probe files left behind: {after:?}"
    );
}

#[tokio::test]
async fn slow_checks_are_cut_off_without_holding_the_request() {
    let start = tokio::time::Instant::now();
    let jobs: Vec<Job> = vec![
        (
            "fast",
            "fast",
            Box::new(|| Check::ok("fast", "fast", "done")),
        ),
        (
            "slow",
            "slow",
            Box::new(|| {
                std::thread::sleep(Duration::from_millis(800));
                Check::ok("slow", "slow", "late")
            }),
        ),
        ("panics", "panics", Box::new(|| panic!("boom"))),
    ];
    let out = run_jobs(
        jobs,
        Duration::from_millis(100),
        start + Duration::from_secs(5),
        &free_permit(),
    )
    .await;
    assert!(
        start.elapsed() < Duration::from_millis(700),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(
        out.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["fast", "slow", "panics"]
    );
    assert_eq!(out[0].status, Status::Ok);
    assert_eq!(out[1].status, Status::Warn);
    assert!(out[1].detail.contains("in time"));
    assert_eq!(out[2].status, Status::Warn);
    // The overall deadline wins over a longer per-check budget.
    let start = tokio::time::Instant::now();
    let jobs: Vec<Job> = vec![(
        "slow",
        "slow",
        Box::new(|| {
            std::thread::sleep(Duration::from_millis(800));
            Check::ok("slow", "slow", "late")
        }),
    )];
    let out = run_jobs(
        jobs,
        Duration::from_secs(3),
        start + Duration::from_millis(100),
        &free_permit(),
    )
    .await;
    assert_eq!(out[0].status, Status::Warn);
    assert!(start.elapsed() < Duration::from_millis(700));
}

#[tokio::test]
async fn a_timed_out_check_keeps_the_diagnostics_slot_until_it_really_ends() {
    let t = setup();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let jobs: Vec<Job> = vec![(
        "stuck",
        "stuck",
        Box::new(move || {
            // A dead mount / keyring: blocks until released.
            let _ = rx.recv_timeout(Duration::from_secs(10));
            Check::ok("stuck", "stuck", "late")
        }),
    )];
    let start = tokio::time::Instant::now();
    let run = permit(&t);
    let out = run_jobs(
        jobs,
        Duration::from_millis(50),
        start + Duration::from_secs(5),
        &run,
    )
    .await;
    drop(run);
    assert_eq!(out[0].status, Status::Warn);
    // The request has answered, but the stuck job still holds the slot: new
    // runs are refused instead of stacking more stuck threads.
    let r = call(&t, Method::GET, "/v1/doctor", None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{:?}", r.json());
    let r = call(&t, Method::GET, "/v1/doctor/report", None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // Once the job ends, the slot is free again.
    tx.send(()).unwrap();
    let mut freed = false;
    for _ in 0..200 {
        if t.state.doctor_permit().is_ok() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(freed, "the slot was not released after the job ended");
}

#[tokio::test]
async fn blocking_cli_discovery_cannot_stall_the_async_worker() {
    // `#[tokio::test]` is single-threaded: a future that blocks in `poll`
    // (synchronous file I/O on a dead UNC path / mount) would stall the whole
    // runtime and its timer if it were polled there.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let start = tokio::time::Instant::now();
    let got = on_blocking_pool(
        move || async move {
            let _ = rx.recv_timeout(Duration::from_secs(5));
            "detected"
        },
        start + Duration::from_millis(100),
        &free_permit(),
    )
    .await;
    assert_eq!(got, None);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the deadline did not hold: {:?}",
        start.elapsed()
    );
    tx.send(()).ok();
    // A quick future still answers, with async parts (timers) working.
    let got = on_blocking_pool(
        || async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            7
        },
        tokio::time::Instant::now() + Duration::from_secs(5),
        &free_permit(),
    )
    .await;
    assert_eq!(got, Some(7));
}

#[tokio::test]
async fn run_checks_keeps_blocking_cli_detection_off_the_async_worker() {
    // `#[tokio::test]` is single-threaded.  The hook blocks synchronously
    // inside the detection future, as file-system calls on a dead UNC path
    // or mount do; if `run_checks` polled detection on the async worker, the
    // whole runtime (and its deadline timer) would stall with it.
    let t = setup();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = parking_lot::Mutex::new(rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    t.state.set_doctor_detect_hook(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        let _ = rx.lock().recv_timeout(Duration::from_secs(5));
    }));
    let start = tokio::time::Instant::now();
    let checks = crate::doctor_routes::run_checks(
        &t.state,
        start + Duration::from_millis(500),
        &free_permit(),
    )
    .await;
    let took = start.elapsed();
    tx.send(()).ok();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "detection did not run");
    assert!(
        took < Duration::from_secs(3),
        "run_checks stalled: {took:?}"
    );
    let cli = checks.iter().find(|c| c.id == "cli_accounts").unwrap();
    assert_eq!(cli.status, Status::Warn, "{}", cli.detail);
    assert!(cli.detail.contains("did not answer"), "{}", cli.detail);
}

#[tokio::test]
async fn unknown_repairs_are_rejected_and_never_run() {
    let t = setup();
    for id in [
        "regenerate_api_token",
        "CREATE_DATA_DIR",
        "shell",
        "..%2F..%2Fx",
    ] {
        let r = call(&t, Method::POST, &format!("/v1/doctor/repair/{id}"), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{id}: {:?}", r.json());
    }
    assert_eq!(audit_actions(&t), []);
    // GET is not a way to run a repair either.
    let r = call(&t, Method::GET, "/v1/doctor/repair/create_data_dir", None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
}

/// The browser download is the repair that costs the most: a holder of only the API token (a CLI
/// engine, a script, a model's shell) cannot start it, and nothing is audited or run.
#[tokio::test]
async fn installing_the_browser_needs_the_owner_key() {
    let t = setup();
    for id in ["install_browser", "create_data_dir"] {
        let r = call_token_only(&t, Method::POST, &format!("/v1/doctor/repair/{id}"), None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{id}: {:?}", r.json());
        assert_eq!(
            r.json()["error"]["code"],
            crate::owner::OWNER_REQUIRED,
            "{id}"
        );
    }
    assert_eq!(audit_trail(&t), Vec::<(String, String, String)>::new());
    assert!(!crate::browser_install::root_dir(&t.state.data_dir()).exists());
}

/// A second install while one runs is refused with 409 and nothing is downloaded; the refusal is
/// audited like any repair outcome.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn a_second_browser_install_at_the_same_time_is_refused_with_409_and_audited() {
    let t = setup();
    let held = crate::browser_install::Running::start(&t.state.data_dir()).unwrap();
    let r = call(&t, Method::POST, "/v1/doctor/repair/install_browser", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("already being installed")),
        "{:?}",
        r.json()
    );
    assert!(!crate::browser_install::root_dir(&t.state.data_dir()).exists());
    let trail = audit_trail(&t);
    assert_eq!(
        trail
            .iter()
            .map(|(a, _, r)| (a.as_str(), r.as_str()))
            .collect::<Vec<_>>(),
        [
            ("doctor.repair_requested", "install_browser"),
            ("doctor.repair", "install_browser")
        ]
    );
    let done = audit_actions(&t);
    assert_eq!(done[0].1["ok"], false);
    drop(held);
}

#[tokio::test]
async fn allow_listed_repairs_run_and_are_audited() {
    let t = setup();
    let r = call(&t, Method::POST, "/v1/doctor/repair/create_data_dir", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["repair"], "create_data_dir");

    // No provider configuration in this daemon: refused, still audited.
    let r = call(&t, Method::POST, "/v1/doctor/repair/reload_providers", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.json());
    // No embedder host: the browser cannot be restarted.
    let r = call(&t, Method::POST, "/v1/doctor/repair/restart_browser", None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let audit = audit_actions(&t);
    let got: Vec<(String, bool)> = audit
        .iter()
        .map(|(_, d)| {
            (
                d["repair"].as_str().unwrap().to_owned(),
                d["ok"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("create_data_dir".to_owned(), true),
            ("reload_providers".to_owned(), false),
            ("restart_browser".to_owned(), false),
        ]
    );
}

#[tokio::test]
async fn repairs_are_audited_as_requested_before_they_run_with_the_initiator() {
    let t = setup();
    let r = call(&t, Method::POST, "/v1/doctor/repair/create_data_dir", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let local = format!("local-ui:{}", t.state.user_id());
    assert_eq!(
        audit_trail(&t),
        [
            (
                "doctor.repair_requested".to_owned(),
                local.clone(),
                "create_data_dir".to_owned()
            ),
            (
                "doctor.repair".to_owned(),
                local,
                "create_data_dir".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn a_repair_that_cannot_be_audited_does_not_run() {
    let t = setup();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let data_dir = t.state.data_dir();
    t.state.set_provider_host(crate::ProviderHost {
        catalog: provider_catalog::Catalog::default(),
        reload: Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
            agent_core::build_providers(
                &data_dir,
                &provider_catalog::Catalog::default(),
                &secret_store::MemoryStore::new(),
                &mut |_| {},
            )
        }),
    });
    let host = Arc::new(FakeHost::default());
    t.state.set_doctor_host(host.clone());
    let db = rusqlite::Connection::open(t._dir.path().join("bot.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_doctor_audit BEFORE INSERT ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
    )
    .unwrap();
    for id in [
        "reload_providers",
        "restart_browser",
        "create_data_dir",
        "install_browser",
    ] {
        let r = call(&t, Method::POST, &format!("/v1/doctor/repair/{id}"), None).await;
        assert_eq!(
            r.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{id}: {:?}",
            r.json()
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "provider reload ran unaudited"
    );
    assert_eq!(
        host.restarts.load(Ordering::SeqCst),
        0,
        "browser restart ran unaudited"
    );
}

#[derive(Default)]
struct FakeHost {
    restarts: AtomicUsize,
    redactor: Option<agent_core::SharedRedactor>,
}

#[async_trait::async_trait]
impl DoctorHost for FakeHost {
    fn desktop(&self) -> Option<desktop_api::Capabilities> {
        Some(desktop_api::Capabilities {
            platform: "windows".into(),
            accessibility_tree: true,
            screenshot: true,
            app_launch: true,
            has_display: true,
        })
    }
    fn can_restart_browser(&self) -> bool {
        true
    }
    async fn restart_browser(&self) -> Result<String, String> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok("browser restarted".into())
    }
    fn redactor(&self) -> Option<agent_core::SharedRedactor> {
        self.redactor.clone()
    }
}

#[tokio::test]
async fn embedder_host_adds_desktop_facts_and_the_browser_restart() {
    let t = setup();
    let host = Arc::new(FakeHost::default());
    t.state.set_doctor_host(host.clone());
    let r = call(&t, Method::GET, "/v1/doctor", None).await;
    let v = r.json();
    let desk = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "desktop_automation")
        .unwrap()
        .clone();
    assert_eq!(desk["status"], "ok", "{desk}");
    // Reading the diagnostics never runs a repair.
    assert_eq!(host.restarts.load(Ordering::SeqCst), 0);
    let r = call(&t, Method::POST, "/v1/doctor/repair/restart_browser", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["detail"], "browser restarted");
    assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reload_repair_uses_the_provider_host() {
    let t = setup();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let data_dir = t.state.data_dir();
    t.state.set_provider_host(crate::ProviderHost {
        catalog: provider_catalog::Catalog::default(),
        reload: Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
            agent_core::build_providers(
                &data_dir,
                &provider_catalog::Catalog::default(),
                &secret_store::MemoryStore::new(),
                &mut |_| {},
            )
        }),
    });
    let _ = call(&t, Method::POST, "/v1/doctor/repair/reload_providers", None).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(audit_actions(&t).len(), 1);
}

/// Report text through the route, asserting 200.
async fn report_text(t: &T) -> String {
    let r = call(t, Method::GET, "/v1/doctor/report", None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    String::from_utf8(r.body).unwrap()
}

/// The unredacted detail of check `id` (what the local UI sees).
async fn raw_detail(t: &T, id: &str) -> String {
    let r = call(t, Method::GET, "/v1/doctor", None).await;
    let v = r.json();
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A secret with URL-special characters (the review's example).
const SPECIAL: &str = "tok/en+value?with=sym&bols-1234";
const ENV_ONLY: &str = "env-only-credential-QX7";
const PASSPHRASE: &str = "vault passphrase 9|9";

#[tokio::test]
async fn report_masks_what_the_daemon_redactor_knows_in_every_encoded_form() {
    use base64::Engine as _;
    let t = setup();
    // The production redactor knows secrets that are not in the store
    // (environment credentials, the vault passphrase).
    let redactor = agent_core::SharedRedactor::default();
    for s in [SPECIAL, ENV_ONLY, PASSPHRASE, super::TOKEN] {
        redactor.register(s);
    }
    t.state.set_doctor_host(Arc::new(FakeHost {
        redactor: Some(redactor),
        ..FakeHost::default()
    }));
    let url = "tok%2Fen%2Bvalue%3Fwith%3Dsym%26bols-1234";
    let b64 = base64::engine::general_purpose::STANDARD.encode(SPECIAL);
    let json = serde_json::to_string(PASSPHRASE).unwrap();
    // A malformed config whose parse error quotes the line.
    std::fs::write(
        t.state.data_dir().join("providers.toml"),
        format!(
            "x = {url} {b64} {ENV_ONLY} {json} {} owner@example.com\n",
            super::TOKEN
        ),
    )
    .unwrap();
    // The unredacted result really carries every sentinel...
    let raw = raw_detail(&t, "config").await;
    for planted in [
        url,
        b64.as_str(),
        ENV_ONLY,
        super::TOKEN,
        "owner@example.com",
    ] {
        assert!(
            raw.contains(planted),
            "{planted} not in the raw detail: {raw}"
        );
    }
    // ...and the report carries none of them.
    let text = report_text(&t).await;
    for leaked in [
        url,
        "tok%2F",
        &b64[..16],
        ENV_ONLY,
        "vault passphrase",
        &super::TOKEN[..16],
        "owner@example.com",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked:\n{text}");
    }

    // Values no pattern recognises (no key shape, no URL encoding, short):
    // only the daemon's redactor can mask them.  Custom provider ids are
    // quoted verbatim in the providers check.
    std::fs::remove_file(t.state.data_dir().join("providers.toml")).unwrap();
    let mut cfg = provider_catalog::ProviderConfig::default();
    for id in [ENV_ONLY, PASSPHRASE, SPECIAL] {
        cfg.upsert(provider_catalog::ProviderSettings::new(id, 1));
    }
    cfg.save(&provider_catalog::config_path(&t.state.data_dir()))
        .unwrap();
    let raw = raw_detail(&t, "providers").await;
    for planted in [ENV_ONLY, PASSPHRASE, SPECIAL] {
        assert!(
            raw.contains(planted),
            "{planted} not in the raw detail: {raw}"
        );
    }
    let text = report_text(&t).await;
    assert!(text.contains("not in the catalog"), "{text}");
    for leaked in [ENV_ONLY, "vault passphrase", "tok/en", "sym&bols"] {
        assert!(!text.contains(leaked), "{leaked} leaked:\n{text}");
    }
}

struct FailingStore;

impl SecretStore for FailingStore {
    fn get(&self, _: &str) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        Err(secret_store::SecretError::Unavailable("injected".into()))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), secret_store::SecretError> {
        Err(secret_store::SecretError::Unavailable("injected".into()))
    }
    fn delete(&self, _: &str) -> Result<bool, secret_store::SecretError> {
        Err(secret_store::SecretError::Unavailable("injected".into()))
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        Err(secret_store::SecretError::Unavailable("injected".into()))
    }
}

#[tokio::test]
async fn report_fails_closed_when_the_secrets_cannot_be_read() {
    let t = setup();
    t.state.set_secret_store(Arc::new(FailingStore));
    let r = call(&t, Method::GET, "/v1/doctor/report", None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{:?}", r.json());
    assert!(!String::from_utf8_lossy(&r.body).contains("Praxi diagnostics report"));
}

/// A store that hangs (a locked keyring) until released.
struct HangingStore(parking_lot::Mutex<std::sync::mpsc::Receiver<()>>);

impl SecretStore for HangingStore {
    fn get(&self, _: &str) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str) -> Result<(), secret_store::SecretError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<bool, secret_store::SecretError> {
        Ok(false)
    }
    fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
        let _ = self.0.lock().recv_timeout(Duration::from_secs(10));
        Ok(vec![])
    }
}

#[tokio::test]
async fn report_generation_ends_by_its_deadline_with_a_hung_keyring() {
    let t = setup();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    t.state
        .set_secret_store(Arc::new(HangingStore(parking_lot::Mutex::new(rx))));
    let start = tokio::time::Instant::now();
    let run = permit(&t);
    let r = crate::doctor_routes::build_report(
        &t.state,
        None,
        start + Duration::from_millis(300),
        &run,
    )
    .await;
    drop(run);
    let e = r.expect_err("a hung keyring cannot produce a report");
    assert_eq!(
        axum::response::IntoResponse::into_response(e).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "report deadline not enforced: {:?}",
        start.elapsed()
    );
    // The hung job still holds the slot.
    assert!(t.state.doctor_permit().is_err());
    for _ in 0..10 {
        tx.send(()).ok();
    }
}

const PLANTED_KEY: &str = "pk.planted.7f3a";
const PLANTED_SHAPED: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";

#[tokio::test]
async fn report_is_markdown_and_carries_no_planted_secret_or_home_path() {
    let t = setup();
    let store = Arc::new(secret_store::MemoryStore::new());
    store.set("provider.alpha.api_key", PLANTED_KEY).unwrap();
    t.state.set_secret_store(store);
    let dir = t.state.data_dir();
    // A broken config whose parse error quotes the secrets and an address.
    std::fs::write(
        dir.join("providers.toml"),
        format!("key = {PLANTED_KEY} {PLANTED_SHAPED} owner@example.com\n"),
    )
    .unwrap();

    let r = call(&t, Method::GET, "/v1/doctor/report", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/markdown"),
        "{:?}",
        r.headers
    );
    let text = String::from_utf8(r.body.clone()).unwrap();
    assert!(text.starts_with("# Praxi diagnostics report"), "{text}");
    // The check table survives redaction, one row per check.
    assert!(text.contains("| Status | Check | Detail |"), "{text}");
    assert!(text.contains("| fail | config | "), "{text}");
    assert!(text.contains("| ok | daemon | running"), "{text}");
    assert!(text.contains("providers.toml"), "{text}");
    for leaked in [
        PLANTED_KEY,
        PLANTED_SHAPED,
        "owner@example.com",
        super::TOKEN,
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked:\n{text}");
    }

    // The home directory is replaced by `~` (planted: the data dir's parent).
    let home = dir.parent().unwrap().to_string_lossy().into_owned();
    let text = crate::doctor_routes::build_report(
        &t.state,
        Some(home.clone()),
        tokio::time::Instant::now() + Duration::from_secs(15),
        &permit(&t),
    )
    .await
    .unwrap();
    assert!(!text.contains(&home), "{home} leaked:\n{text}");
    assert!(
        !text.contains(&home.replace('\\', "/")),
        "{home} leaked:\n{text}"
    );
    assert!(text.contains("`~"), "{text}");
}

/// A Full access lane switch the daemon could not read at start is visible in the app's
/// diagnostics, with the reason and the fix (second review, finding 3).
#[tokio::test]
async fn doctor_shows_a_full_access_lane_that_is_off_because_host_toml_was_unreadable() {
    let t = super::setup_with_daemon(
        |_| {},
        agent_core::LoopConfig::default(),
        |cfg| {
            cfg.full_access_lane = agent_core::FullLaneSwitch {
                enabled: false,
                problem: Some("host.toml is not valid: expected `]`".into()),
            };
        },
    );
    let r = call(&t, Method::GET, "/v1/doctor", None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let v = r.json();
    let check = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "full_access_lane")
        .expect("the lane has a row");
    assert_eq!(check["status"], "warn", "{check}");
    assert!(check["detail"].as_str().unwrap().contains("OFF"), "{check}");
    assert!(
        check["detail"].as_str().unwrap().contains("not valid"),
        "{check}"
    );
    assert!(
        check["hint"].as_str().unwrap().contains("host.toml"),
        "{check}"
    );
    // A warning, not a failure: the daemon works, with more cards than the owner chose.
    assert_ne!(check["status"], "fail");
}
