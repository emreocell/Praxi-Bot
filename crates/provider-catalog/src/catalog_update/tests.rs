use std::time::{Duration, UNIX_EPOCH};

use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

use super::*;

const TEST_KEY: &str = "RWQQLCryotKyTZNg2bx02Q0UOnW4T0vHeJavNBaTvyFvgl8cDGQQrATT";

fn fixture(name: &str) -> (Vec<u8>, String) {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/catalog_update/"
    );
    (
        std::fs::read(format!("{dir}{name}.json")).unwrap(),
        std::fs::read_to_string(format!("{dir}{name}.json.minisig")).unwrap(),
    )
}

async fn serve(name: &str) -> (MockServer, String, String) {
    let (data, sig) = fixture(name);
    let server = MockServer::start().await;
    Mock::given(path("/c.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data))
        .mount(&server)
        .await;
    Mock::given(path("/c.minisig"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sig))
        .mount(&server)
        .await;
    let urls = (
        format!("{}/c.json", server.uri()),
        format!("{}/c.minisig", server.uri()),
    );
    (server, urls.0, urls.1)
}

#[test]
fn valid_signature_is_accepted_and_unknown_fields_are_ignored() {
    let (data, sig) = fixture("valid");
    let t = verify_and_parse(&data, &sig, TEST_KEY).unwrap();
    assert_eq!(t.last_reviewed, "2026-12-01");
    // Unknown model kind degrades to "other"; unknown fields did not matter.
    assert_eq!(t.providers[0].models[1].kind, ModelKind::Other);
}

#[test]
fn tampered_file_and_wrong_key_are_refused() {
    let (data, sig) = fixture("tampered");
    assert!(matches!(
        verify_and_parse(&data, &sig, TEST_KEY),
        Err(CatalogUpdateError::BadSignature(_))
    ));
    // A correctly signed file does not verify under the pinned production key.
    let (data, sig) = fixture("valid");
    assert!(matches!(
        verify_and_parse(&data, &sig, PINNED_PUBLIC_KEY),
        Err(CatalogUpdateError::BadSignature(_))
    ));
}

// These fixtures use a throwaway key; the secret key was discarded after signing.
#[test]
fn minisign_algorithm_and_comment_boundaries() {
    let (data, _) = fixture("valid");
    let key = include_str!("../../tests/fixtures/catalog_update/regression.pub").trim();
    let prehashed =
        include_str!("../../tests/fixtures/catalog_update/regression-prehashed.minisig");
    let legacy = include_str!("../../tests/fixtures/catalog_update/regression-legacy.minisig");
    assert!(verify_and_parse(&data, prehashed, key).is_ok());
    assert!(matches!(
        verify_and_parse(&data, legacy, key),
        Err(CatalogUpdateError::BadSignature(_))
    ));

    let mut lines: Vec<_> = prehashed.lines().map(str::to_owned).collect();
    let replacement = if &lines[1][20..21] == "A" { "B" } else { "A" };
    lines[1].replace_range(20..21, replacement);
    let tampered = lines.join("\n");
    assert!(matches!(
        verify_and_parse(&data, &tampered, key),
        Err(CatalogUpdateError::BadSignature(_))
    ));
    let trusted = prehashed.replace("fixture catalog", "fixture altered");
    assert!(matches!(
        verify_and_parse(&data, &trusted, key),
        Err(CatalogUpdateError::BadSignature(_))
    ));
    let untrusted = prehashed.replace(
        "untrusted comment: test fixture",
        "untrusted comment: changed",
    );
    assert!(verify_and_parse(&data, &untrusted, key).is_ok());
}

#[test]
fn oversized_input_is_refused_before_anything_else() {
    let (_, sig) = fixture("valid");
    let big = vec![b' '; MAX_CATALOG_BYTES + 1];
    assert_eq!(
        verify_and_parse(&big, &sig, TEST_KEY),
        Err(CatalogUpdateError::TooLarge)
    );
}

#[test]
fn rolled_back_older_file_is_refused_but_merge_prefers_newer_dates() {
    let bundled = embedded_metadata();
    let (data, sig) = fixture("older");
    let older = verify_and_parse(&data, &sig, TEST_KEY).unwrap(); // genuinely signed
    assert!(matches!(
        check_not_rollback(bundled, &older),
        Err(CatalogUpdateError::Rollback { .. })
    ));

    let (data, sig) = fixture("valid");
    let newer = verify_and_parse(&data, &sig, TEST_KEY).unwrap();
    check_not_rollback(bundled, &newer).unwrap();
    let merged = merge(bundled, &newer).unwrap();
    assert_eq!(merged.last_reviewed, "2026-12-01");
    assert_eq!(
        merged.find_provider("anthropic").unwrap().models[0].id,
        "claude-fixture-new"
    );
    // Providers the update does not mention stay as bundled.
    assert!(merged.find_provider("deepseek").is_some());
    // A provider with an OLDER date than the bundled one does not replace it.
    let mut stale = newer.clone();
    stale.providers[0].last_reviewed = Some("2020-01-01".into());
    let kept = merge(bundled, &stale).unwrap();
    assert_ne!(
        kept.find_provider("anthropic").unwrap().models[0].id,
        "claude-fixture-new"
    );
}

#[test]
fn status_line_shows_date_and_staleness_after_30_days() {
    let t = embedded_metadata();
    let at = |date: &str, plus: i64| {
        let d = IsoDate::parse(date).unwrap();
        let days = d.days_since(IsoDate::parse("2000-01-01").unwrap()) + 10_957 + plus;
        UNIX_EPOCH + Duration::from_secs(u64::try_from(days).unwrap() * 86_400)
    };
    let (line, stale) = status_line(t, Some("2026-12-01"), at("2026-12-01", 10));
    assert_eq!(line, "Catalog updated 2026-12-01");
    assert!(!stale);
    assert!(status_line(t, Some("2026-12-01"), at("2026-12-01", 31)).1);
}

#[tokio::test]
async fn update_installs_then_reports_up_to_date_and_ignores_edited_cache() {
    let (_server, url, sig_url) = serve("valid").await;
    let dir = tempfile::tempdir().unwrap();
    let store = CatalogStore::new(dir.path());

    let check = run_check(&store, false, &url, &sig_url, TEST_KEY, true)
        .await
        .unwrap();
    assert!(matches!(
        check,
        UpdateOutcome::Newer {
            installed: false,
            ..
        }
    ));
    assert!(
        store.load_cached(TEST_KEY).is_none(),
        "--check must not install"
    );

    let done = run_check(&store, true, &url, &sig_url, TEST_KEY, true)
        .await
        .unwrap();
    assert!(
        matches!(done, UpdateOutcome::Newer { installed: true, ref to, .. } if to == "2026-12-01")
    );
    assert_eq!(
        store.load_cached(TEST_KEY).unwrap().last_reviewed,
        "2026-12-01"
    );
    assert!(matches!(
        run_check(&store, true, &url, &sig_url, TEST_KEY, true)
            .await
            .unwrap(),
        UpdateOutcome::UpToDate { .. }
    ));

    // A cache file edited on disk is ignored (re-verified on load).
    let cached = dir.path().join("provider-catalog/provider-catalog.json");
    let text = std::fs::read_to_string(&cached).unwrap();
    std::fs::write(
        &cached,
        text.replace("claude-fixture-new", "claude-evil-model!"),
    )
    .unwrap();
    assert!(store.load_cached(TEST_KEY).is_none());
}

#[tokio::test]
async fn refused_download_leaves_nothing_installed() {
    let (_server, url, sig_url) = serve("tampered").await;
    let dir = tempfile::tempdir().unwrap();
    let store = CatalogStore::new(dir.path());
    let err = run_check(&store, true, &url, &sig_url, TEST_KEY, true)
        .await
        .unwrap_err();
    assert!(matches!(err, CatalogUpdateError::BadSignature(_)));
    assert!(store.load_cached(TEST_KEY).is_none());
    // Plain http is refused for the real entry point.
    assert!(matches!(
        fetch("http://example.com/x", 10, false).await,
        Err(CatalogUpdateError::Network(_))
    ));
}

#[test]
fn settings_default_on_and_can_be_switched_off_and_daily_gate_works() {
    let dir = tempfile::tempdir().unwrap();
    let store = CatalogStore::new(dir.path());
    assert!(store.auto_update_enabled());
    store.set_auto_update(false).unwrap();
    assert!(!store.auto_update_enabled());
    store.set_auto_update(true).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(10_000_000);
    assert!(store.check_due(now));
    store.save_state(&CatalogState {
        last_checked: 10_000_000 - 3_600,
        ..Default::default()
    });
    assert!(!store.check_due(now));
    store.save_state(&CatalogState {
        last_checked: 10_000_000 - 90_000,
        ..Default::default()
    });
    assert!(store.check_due(now));
}
