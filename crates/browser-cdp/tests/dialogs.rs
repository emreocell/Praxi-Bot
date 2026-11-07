//! Fixture tests for JS dialogs, network idle and redundant waits. Need Chrome/Edge:
//! `cargo test -p browser-cdp --test dialogs -- --ignored --test-threads=1`

mod common;

use std::time::{Duration, Instant};

use browser_cdp::{Browser, DialogOptions, LaunchOptions, Page, Snapshot, SnapshotOptions};

fn button(snap: &Snapshot) -> String {
    snap.nodes
        .iter()
        .find(|n| n.role == "button" && n.ref_id.is_some())
        .and_then(|n| n.ref_id.clone())
        .unwrap_or_else(|| panic!("no button in:\n{}", snap.render()))
}

async fn setup(path: &str) -> (Browser, Page, String) {
    let port = common::serve().await;
    let base = format!("http://127.0.0.1:{port}");
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b.new_page(&format!("{base}{path}")).await.unwrap();
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let r = button(&snap);
    (b, p, r)
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn alert_is_accepted_quickly_and_reported() {
    let (browser, page, button_ref) = setup("/alert").await;
    let start = Instant::now();
    let res = page.click(&button_ref).await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(res.dialogs.len(), 1);
    let dialog = &res.dialogs[0];
    assert_eq!(
        (dialog.kind.as_str(), dialog.message.as_str()),
        ("alert", "hello")
    );
    assert!(dialog.accepted);
    assert_eq!(page.title().await.unwrap(), "after-alert");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn two_dialogs_during_one_action_are_all_reported() {
    let (browser, page, button_ref) = setup("/two").await;
    let start = Instant::now();
    let res = page.click(&button_ref).await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(res.dialogs.len(), 2, "must report all dialogs");
    assert_eq!(
        (
            res.dialogs[0].kind.as_str(),
            res.dialogs[0].message.as_str(),
            res.dialogs[0].accepted
        ),
        ("alert", "first", true)
    );
    assert_eq!(
        (
            res.dialogs[1].kind.as_str(),
            res.dialogs[1].message.as_str(),
            res.dialogs[1].accepted
        ),
        ("alert", "second", true)
    );
    assert_eq!(page.title().await.unwrap(), "both");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn confirm_is_dismissed_and_page_sees_false() {
    let (browser, page, button_ref) = setup("/confirm").await;
    let start = Instant::now();
    let res = page.click(&button_ref).await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(res.dialogs.len(), 1);
    let dialog = &res.dialogs[0];
    assert_eq!(dialog.kind, "confirm");
    assert!(!dialog.accepted);
    assert_eq!(page.title().await.unwrap(), "confirm:false");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn confirm_is_accepted_with_option() {
    let (browser, page, button_ref) = setup("/confirm").await;
    let res = page
        .click_with(&button_ref, &DialogOptions::accept())
        .await
        .unwrap();
    assert_eq!(res.dialogs.len(), 1);
    assert_eq!(res.dialogs[0].kind, "confirm");
    assert!(res.dialogs[0].accepted);
    assert_eq!(page.title().await.unwrap(), "confirm:true");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn prompt_is_dismissed_by_default() {
    let (browser, page, button_ref) = setup("/prompt").await;
    let res = page.click(&button_ref).await.unwrap();
    assert_eq!(res.dialogs.len(), 1);
    assert_eq!(res.dialogs[0].kind, "prompt");
    assert!(!res.dialogs[0].accepted);
    assert_eq!(page.title().await.unwrap(), "prompt:null");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn prompt_is_accepted_with_prompt_text() {
    let (browser, page, button_ref) = setup("/prompt").await;
    let res = page
        .click_with(&button_ref, &DialogOptions::accept_prompt("Alice"))
        .await
        .unwrap();
    assert_eq!(res.dialogs.len(), 1);
    assert_eq!(res.dialogs[0].kind, "prompt");
    assert!(res.dialogs[0].accepted);
    assert_eq!(page.title().await.unwrap(), "prompt:Alice");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn navigating_away_from_beforeunload_dismissed_by_default() {
    let (browser, page, button_ref) = setup("/beforeunload").await;
    let _ = page.click(&button_ref).await.unwrap(); // user activation arms the handler
    let form_url = format!(
        "{}/form",
        page.url().await.unwrap().trim_end_matches("/beforeunload")
    );
    let res = page.navigate(&form_url).await.unwrap();
    assert!(
        !res.ok,
        "default dismisses beforeunload so navigation aborts"
    );
    assert_eq!(res.dialogs.len(), 1);
    assert_eq!(res.dialogs[0].kind, "beforeunload");
    assert!(!res.dialogs[0].accepted);
    assert!(
        res.snapshot.url.ends_with("/beforeunload"),
        "stays on beforeunload page: {}",
        res.snapshot.url
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn navigating_away_from_beforeunload_accepted_with_option() {
    let (browser, page, button_ref) = setup("/beforeunload").await;
    let _ = page.click(&button_ref).await.unwrap(); // user activation arms the handler
    let form_url = format!(
        "{}/form",
        page.url().await.unwrap().trim_end_matches("/beforeunload")
    );
    let res = page
        .navigate_with(&form_url, &DialogOptions::accept())
        .await
        .unwrap();
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.dialogs.len(), 1);
    assert_eq!(res.dialogs[0].kind, "beforeunload");
    assert!(res.dialogs[0].accepted);
    assert!(res.snapshot.url.ends_with("/form"), "{}", res.snapshot.url);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn redirect_chain_does_not_hit_the_idle_cap() {
    let port = common::serve().await;
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = browser
        .new_page(&format!("http://127.0.0.1:{port}/form"))
        .await
        .unwrap();
    page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let start = Instant::now();
    let res = page
        .navigate(&format!("http://127.0.0.1:{port}/redir"))
        .await
        .unwrap();
    assert!(res.settle_note.is_none(), "{:?}", res.settle_note);
    assert!(
        start.elapsed() < Duration::from_millis(1800),
        "{:?}",
        start.elapsed()
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn hash_back_completes_fast() {
    let (browser, page, button_ref) = setup("/spa").await;
    page.click(&button_ref).await.unwrap();
    let start = Instant::now();
    let res = page.back().await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert!(
        res.ok && !res.snapshot.url.contains("#s"),
        "{}",
        res.snapshot.url
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn open_takes_one_snapshot_round_and_matches_a_plain_snapshot() {
    let port = common::serve().await;
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = browser.new_page("about:blank").await.unwrap();
    let opts = SnapshotOptions::default();
    let before = page.snapshot_rounds();
    let res = page
        .navigate_snapshot(&format!("http://127.0.0.1:{port}/form"), &opts)
        .await
        .unwrap();
    assert_eq!(page.snapshot_rounds() - before, 1);
    let again = page.snapshot(&opts).await.unwrap();
    assert_eq!(res.snapshot.render(), again.render());
    browser.close().await.unwrap();
}
