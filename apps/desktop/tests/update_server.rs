//! The update plugin glue against a mock update server on loopback: an
//! installer whose signature does not verify is never installed, and a rollback
//! only ever goes to an older version.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use desktop_lib::update::{UpdateRuntime, UpdateSettings, check, install, pick_notes};
use tauri::Url;

const BODY: &str =
    "Default\n<!-- lang: en -->\nFixed the thing.\n<!-- lang: tr -->\nSorun düzeltildi.\n";

/// Serves `/latest.json` (announcing `version`, empty signature) and `/setup.exe`.
fn mock_server(version: &'static str) -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let installer_hits = Arc::new(AtomicUsize::new(0));
    let hits = Arc::clone(&installer_hits);
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
    )
}

fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        serde_json::json!({
            "pubkey": "",
            "endpoints": [],
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
    }
}

#[tokio::test]
async fn an_update_without_a_valid_signature_is_never_installed() {
    let app = mock_app();
    let (endpoint, installer_hits) = mock_server("99.0.0");
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
    let (older, _) = mock_server("0.0.0-rollback");
    let s = settings(older.clone());
    let found = check(app.handle(), &s, &older, true).await.expect("check");
    assert!(
        found.is_some_and(|u| u.version == "0.0.0-rollback"),
        "an older release is offered as rollback"
    );
    let (newer, _) = mock_server("99.0.0");
    let found = check(app.handle(), &s, &newer, true).await.expect("check");
    assert!(found.is_none(), "a newer release is never a rollback");
    let found = check(app.handle(), &s, &older, false).await.expect("check");
    assert!(found.is_none(), "a normal update check never downgrades");
}
