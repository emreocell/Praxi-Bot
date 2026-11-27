//! The update plugin glue against a mock update server on loopback: an
//! installer whose signature does not verify is never installed, and a rollback
//! only ever goes to an older version.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use desktop_lib::update::{
    UpdateChannel, UpdateRuntime, UpdateSettings, check, check_for_update, install, pick_notes,
    set_channel, skip_version, status,
};
use tauri::Url;

const BODY: &str = "Default\n<!-- lang: en -->\nFixed the thing.\n<!-- lang: tr -->\nSorun düzeltildi.\n<!-- lang: de -->\nProblem behoben.\n";

/// Serves `/latest.json` (announcing `version`, empty signature) and `/setup.exe`.
fn mock_server(version: &'static str) -> (Url, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let installer_hits = Arc::new(AtomicUsize::new(0));
    let hits = Arc::clone(&installer_hits);
    let feed_hits = Arc::new(AtomicUsize::new(0));
    let feeds = Arc::clone(&feed_hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let (ctype, body) = if request.starts_with("GET /setup.exe") {
                hits.fetch_add(1, Ordering::SeqCst);
                ("application/octet-stream", b"MZ-not-signed".to_vec())
            } else {
                feeds.fetch_add(1, Ordering::SeqCst);
                let entry = serde_json::json!({
                    "url": format!("http://127.0.0.1:{port}/setup.exe"),
                    "signature": "",
                });
                let mut platforms = serde_json::Map::new();
                for os in ["windows", "linux", "darwin"] {
                    for arch in ["x86_64", "aarch64", "i686", "armv7"] {
                        platforms.insert(format!("{os}-{arch}"), entry.clone());
                    }
                }
                let manifest = serde_json::json!({
                    "version": version,
                    "notes": BODY,
                    "platforms": platforms,
                });
                ("application/json", manifest.to_string().into_bytes())
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (
        Url::parse(&format!("http://127.0.0.1:{port}/latest.json")).expect("url"),
        installer_hits,
        feed_hits,
    )
}

fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    mock_app_version("0.1.0")
}

fn mock_app_version(version: &str) -> tauri::App<tauri::test::MockRuntime> {
    mock_app_with(version, false)
}

fn mock_app_with(
    version: &str,
    require_signed_version: bool,
) -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.package_info_mut().version = version.parse().expect("version");
    context.config_mut().plugins.0.insert(
        "updater".into(),
        serde_json::json!({
            "pubkey": "",
            "endpoints": [],
            "requireSignedVersion": require_signed_version,
            "dangerousInsecureTransportProtocol": true
        }),
    );
    tauri::test::mock_builder()
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey("not-a-real-key")
                .build(),
        )
        .build(context)
        .expect("mock app")
}

fn settings(endpoint: Url) -> UpdateSettings {
    UpdateSettings {
        pubkey: "not-a-real-key".into(),
        endpoint,
        rollback_endpoint: None,
        preview_endpoint: None,
    }
}

#[tokio::test]
async fn an_update_without_a_valid_signature_is_never_installed() {
    let app = mock_app();
    let (endpoint, installer_hits, _) = mock_server("99.0.0");
    let s = settings(endpoint.clone());
    let update = check(app.handle(), &s, &endpoint, false)
        .await
        .expect("check")
        .expect("a newer version is announced");
    assert_eq!(update.version, "99.0.0");
    assert_eq!(
        pick_notes(update.body.as_deref().unwrap_or(""), "tr"),
        "Sorun düzeltildi."
    );

    let rt = UpdateRuntime::detached(Some(s), "0.1.0");
    let err = install(&rt, &update).await.expect_err("must refuse");
    assert_eq!(err, "signature");
    assert_eq!(
        installer_hits.load(Ordering::SeqCst),
        1,
        "installer was fetched"
    );
    assert_eq!(rt.pending(), None, "nothing was recorded as installed");
}

#[tokio::test]
async fn rollback_only_goes_to_an_older_version() {
    let app = mock_app();
    let (older, _, _) = mock_server("0.0.0-rollback");
    let s = settings(older.clone());
    let found = check(app.handle(), &s, &older, true).await.expect("check");
    assert!(
        found.is_some_and(|u| u.version == "0.0.0-rollback"),
        "an older release is offered as rollback"
    );
    let (newer, _, _) = mock_server("99.0.0");
    let found = check(app.handle(), &s, &newer, true).await.expect("check");
    assert!(found.is_none(), "a newer release is never a rollback");
    let found = check(app.handle(), &s, &older, false).await.expect("check");
    assert!(found.is_none(), "a normal update check never downgrades");
}

#[tokio::test]
async fn preview_requires_opt_in_and_checks_and_skips_never_download() {
    let app = mock_app_version("0.1.0-alpha.1");
    // Even a misconfigured stable feed cannot offer a prerelease.
    let (stable, stable_downloads, _) = mock_server("0.1.0-alpha.2");
    let (preview, preview_downloads, preview_hits) = mock_server("0.1.0-alpha.3");
    let mut s = settings(stable);
    s.preview_endpoint = Some(preview);
    let mut rt = UpdateRuntime::detached(Some(s), "0.1.0-alpha.1");
    assert_eq!(status(&rt, "en").unwrap().channel, UpdateChannel::Stable);
    assert!(
        check_for_update(app.handle(), &rt, "en")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(preview_hits.load(Ordering::SeqCst), 0);
    assert!(status(&rt, "en").unwrap().last_checked.is_some());

    set_channel(&rt, UpdateChannel::Preview).unwrap();
    for (lang, expected) in [
        ("en", "Fixed the thing."),
        ("tr", "Sorun düzeltildi."),
        ("de", "Problem behoben."),
    ] {
        let found = check_for_update(app.handle(), &rt, lang)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.version, "0.1.0-alpha.3");
        assert_eq!(found.notes, expected);
    }
    skip_version(&rt).unwrap();
    assert_eq!(
        status(&rt, "en").unwrap().skipped_version.as_deref(),
        Some("0.1.0-alpha.3")
    );
    assert!(
        check_for_update(app.handle(), &rt, "en")
            .await
            .unwrap()
            .is_none()
    );
    let (newer, newer_downloads, _) = mock_server("0.1.0-alpha.4");
    rt.settings.as_mut().unwrap().preview_endpoint = Some(newer);
    assert_eq!(
        check_for_update(app.handle(), &rt, "en")
            .await
            .unwrap()
            .unwrap()
            .version,
        "0.1.0-alpha.4"
    );
    assert_eq!(newer_downloads.load(Ordering::SeqCst), 0);
    let (invalid, _, _) = mock_server("invalid-version");
    rt.settings.as_mut().unwrap().preview_endpoint = Some(invalid);
    assert!(check_for_update(app.handle(), &rt, "en").await.is_err());
    assert!(
        skip_version(&rt).is_err(),
        "a failed check clears the previous candidate"
    );

    set_channel(&rt, UpdateChannel::Stable).unwrap();
    assert!(status(&rt, "en").unwrap().skipped_version.is_none());
    assert!(
        skip_version(&rt).is_err(),
        "changing channels clears the installer candidate"
    );
    assert_eq!(stable_downloads.load(Ordering::SeqCst), 0);
    assert_eq!(preview_downloads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_signing_key_disables_checks_and_preview_has_no_stable_fallback() {
    let app = mock_app();
    let rt = UpdateRuntime::detached(None, "0.1.0");
    assert_eq!(
        check_for_update(app.handle(), &rt, "en").await.unwrap_err(),
        "not_configured"
    );
    assert!(status(&rt, "en").unwrap().last_checked.is_none());
    let (endpoint, _, hits) = mock_server("99.0.0");
    let rt = UpdateRuntime::detached(Some(settings(endpoint)), "0.1.0");
    assert_eq!(
        set_channel(&rt, UpdateChannel::Preview).unwrap_err(),
        "preview_not_configured"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert_eq!(status(&rt, "en").unwrap().channel, UpdateChannel::Stable);
}

// ------------------------------------------------- real signatures (fixtures)

/// Public keys and signatures made with two throwaway Tauri signing keys (private halves
/// discarded), shared with `scripts/tests/test_update_manifest.py`. `installer_bytes` is the
/// file every signature below was made for.
fn fixtures() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../scripts/tests/fixtures/update_signatures.json"
    ))
    .expect("fixtures")
}

/// Serves a feed announcing `version` whose installer is `installer` with `signature`.
fn signed_server(version: &'static str, signature: String, installer: Vec<u8>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let (ctype, body) = if request.starts_with("GET /setup.exe") {
                ("application/octet-stream", installer.clone())
            } else {
                // Every target the updater may name, so the test runs on any OS (not only Windows).
                let entry = serde_json::json!({
                    "url": format!("http://127.0.0.1:{port}/setup.exe"),
                    "signature": signature,
                });
                let mut platforms = serde_json::Map::new();
                for os in ["windows", "linux", "darwin"] {
                    for arch in ["x86_64", "aarch64", "i686", "armv7"] {
                        platforms.insert(format!("{os}-{arch}"), entry.clone());
                    }
                }
                let manifest = serde_json::json!({
                    "version": version,
                    "notes": BODY,
                    "pub_date": "2026-11-27T08:15:00Z",
                    "platforms": platforms,
                });
                ("application/json", manifest.to_string().into_bytes())
            };
            let head = format!(
                "HTTP/1.1 200 OK
Content-Type: {ctype}
Content-Length: {}
Connection: close

",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Url::parse(&format!("http://127.0.0.1:{port}/latest.json")).expect("url")
}

/// Runs the update flow up to (not including) the installer start, as the app does it.
async fn download_from_feed(
    pubkey_of: &str,
    signature_of: &str,
    announced: &'static str,
) -> Result<Vec<u8>, String> {
    let f = fixtures();
    let installer = f["installer_bytes"]
        .as_str()
        .expect("bytes")
        .as_bytes()
        .to_vec();
    let endpoint = signed_server(
        announced,
        f[signature_of].as_str().expect("sig").to_owned(),
        installer,
    );
    let s = UpdateSettings {
        pubkey: f[pubkey_of].as_str().expect("key").to_owned(),
        endpoint: endpoint.clone(),
        rollback_endpoint: None,
        preview_endpoint: None,
    };
    let app = mock_app_with("0.1.0", true);
    let update = check(app.handle(), &s, &endpoint, false)
        .await
        .map_err(|e| format!("check: {e}"))?
        .ok_or("no update announced")?;
    assert_eq!(update.version, announced);
    update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_genuine_signed_update_downloads_and_the_feed_we_publish_is_accepted() {
    // The signature in the feed was made by the key whose public half is built into the app,
    // and is bound to the announced version: exactly what the release workflow publishes.
    let bytes = download_from_feed("pub_a", "a_stable", "0.2.0")
        .await
        .expect("a valid update downloads");
    assert_eq!(
        bytes,
        fixtures()["installer_bytes"].as_str().unwrap().as_bytes()
    );
}

#[tokio::test]
async fn a_signature_from_another_key_or_for_another_version_is_refused() {
    // Another key (the right version): refused by the signature check.
    let err = download_from_feed("pub_b", "a_stable", "0.2.0")
        .await
        .unwrap_err();
    assert!(
        err.to_lowercase().contains("signature") || err.contains("key"),
        "{err}"
    );
    // A genuine signature of an OLDER release, replayed under a newer announced version.
    let err = download_from_feed("pub_a", "a_old", "0.2.0")
        .await
        .unwrap_err();
    assert!(err.contains("signed for version"), "{err}");
    // A signature that carries no version is refused because requireSignedVersion is on.
    let err = download_from_feed("pub_a", "a_legacy", "0.2.0")
        .await
        .unwrap_err();
    assert!(err.contains("requireSignedVersion"), "{err}");
}
