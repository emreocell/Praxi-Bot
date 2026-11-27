//! Files in and out of the browser, tabs, reload and forward, on a real Chrome/Edge and a local
//! fixture site. Run with:
//! `cargo test -p browser-cdp --test files_and_tabs -- --ignored --test-threads=1`

use std::path::Path;
use std::time::Duration;

use browser_cdp::{Browser, DownloadPolicy, LaunchOptions, Page, Snapshot, SnapshotOptions};

mod common;
use common::serve;

fn ref_named(snap: &Snapshot, needle: &str) -> String {
    snap.nodes
        .iter()
        .find(|n| n.name.contains(needle) && n.ref_id.is_some())
        .and_then(|n| n.ref_id.clone())
        .unwrap_or_else(|| panic!("no element named {needle:?} in:\n{}", snap.render()))
}

async fn snap(page: &Page) -> Snapshot {
    page.snapshot(&SnapshotOptions {
        compact: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn text_of(page: &Page, id: &str) -> String {
    page.evaluate(&format!("document.getElementById('{id}').textContent"))
        .await
        .unwrap()
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Poll `check` until it returns `Some`, up to `secs` seconds.
async fn until<T>(secs: u64, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..secs * 10 {
        if let Some(v) = check() {
            return Some(v);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

async fn open_files(b: &Browser, port: u16) -> Page {
    b.new_page(&format!("http://127.0.0.1:{port}/files"))
        .await
        .expect("open the fixture")
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_file_input_takes_a_file_and_the_page_reads_its_content() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open_files(&b, port).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello from the bot").unwrap();
    let r = page
        .upload_file(&ref_named(&snap(&page).await, "Pick"), &[file])
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let mut shown = String::new();
    for _ in 0..30 {
        shown = text_of(&page, "out").await;
        if shown.starts_with("picked:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown, "picked:note.txt:hello from the bot");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn several_files_go_into_an_input_that_takes_several() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open_files(&b, port).await;
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<_> = ["a.txt", "b.txt"]
        .iter()
        .map(|n| {
            let p = dir.path().join(n);
            std::fs::write(&p, n).unwrap();
            p
        })
        .collect();
    let r = page
        .upload_file(&ref_named(&snap(&page).await, "Many"), &files)
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let mut shown = String::new();
    for _ in 0..30 {
        shown = text_of(&page, "out").await;
        if shown.starts_with("many:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown, "many:a.txt,b.txt");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn an_element_that_is_not_a_file_input_is_not_given_a_file() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open_files(&b, port).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "x").unwrap();
    let r = page
        .upload_file(&ref_named(&snap(&page).await, "Get hello"), &[file])
        .await
        .unwrap();
    assert!(!r.ok, "a link took a file");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn downloads_are_refused_until_a_folder_is_allowed_and_the_folder_is_the_only_place() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    assert!(b.downloads().is_none(), "nothing is kept by default");
    let page = open_files(&b, port).await;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("inbox");
    b.allow_downloads(DownloadPolicy::new(&dir)).await.unwrap();
    // The same folder again changes nothing.
    b.allow_downloads(DownloadPolicy::new(&dir)).await.unwrap();

    let s = snap(&page).await;
    page.click(&ref_named(&s, "Get hello")).await.unwrap();
    page.click(&ref_named(&s, "Get evil")).await.unwrap();
    let report = until(20, || {
        let r = b.downloads()?;
        (r.finished.len() == 2 && r.running.is_empty()).then_some(r)
    })
    .await
    .unwrap_or_else(|| panic!("downloads did not finish: {:?}", b.downloads()));

    let names: Vec<_> = report.finished.iter().map(|d| d.name.as_str()).collect();
    // The page asked for `../../evil.txt`: it lands inside the folder, with no folder part.
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"hello.txt"), "{names:?}");
    let evil = names
        .iter()
        .find(|n| n.ends_with("evil.txt"))
        .unwrap_or_else(|| panic!("{names:?}"));
    assert!(!evil.contains(['/', '\\']), "{evil}");
    let hello = report
        .finished
        .iter()
        .find(|d| d.name == "hello.txt")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&hello.path).unwrap(),
        "hello download"
    );
    assert_eq!(hello.size, "hello download".len() as u64);
    assert_eq!(hello.mime, "text/plain");
    assert_eq!(
        hello.from.as_deref(),
        Some(&*format!("http://127.0.0.1:{port}"))
    );
    // Nothing else exists: no browser-named leftovers, nothing outside the folder.
    let mut inside: Vec<_> = names.iter().map(ToString::to_string).collect();
    inside.sort();
    assert_eq!(names_in(&dir), inside);
    assert_eq!(names_in(root.path()), ["inbox"]);
    // The page stays usable after a download.
    assert!(text_of(&page, "out").await.contains("none"));
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_download_over_the_size_cap_is_cancelled_and_nothing_is_kept() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open_files(&b, port).await;
    let root = tempfile::tempdir().unwrap();
    let policy = DownloadPolicy {
        max_file_bytes: 1000,
        ..DownloadPolicy::new(root.path().join("inbox"))
    };
    b.allow_downloads(policy).await.unwrap();
    let s = snap(&page).await;
    page.click(&ref_named(&s, "Get big")).await.unwrap();
    let report = until(20, || b.downloads().filter(|r| !r.refused.is_empty()))
        .await
        .unwrap_or_else(|| panic!("nothing was refused: {:?}", b.downloads()));
    assert!(
        report.refused[0].contains("big.bin"),
        "{:?}",
        report.refused
    );
    assert!(report.refused[0].contains("limit"), "{:?}", report.refused);
    assert!(report.finished.is_empty(), "{:?}", report.finished);
    // Give the cleanup a moment, then the folder is empty.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(names_in(&root.path().join("inbox")), Vec::<String>::new());
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_tab_opens_on_an_address_becomes_current_and_closes() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let first = open_files(&b, port).await;
    let before = b.pages().await.unwrap().len();
    let second = b
        .new_page(&format!("http://127.0.0.1:{port}/second"))
        .await
        .unwrap();
    let tabs = b.pages().await.unwrap();
    assert_eq!(tabs.len(), before + 1, "{tabs:?}");
    assert!(tabs.iter().any(|t| t.url.ends_with("/second")), "{tabs:?}");
    // The new tab reads on its own; the first one is untouched.
    assert!(snap(&second).await.render().contains("Second tab"));
    assert!(snap(&first).await.render().contains("Files"));
    b.close_page(second.target_id()).await.unwrap();
    let mut tabs = b.pages().await.unwrap();
    for _ in 0..30 {
        if tabs.len() == before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        tabs = b.pages().await.unwrap();
    }
    assert_eq!(tabs.len(), before, "{tabs:?}");
    assert!(tabs.iter().all(|t| !t.url.ends_with("/second")), "{tabs:?}");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn forward_returns_to_the_page_back_left_and_reload_loads_the_page_again() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open_files(&b, port).await;
    page.navigate(&format!("http://127.0.0.1:{port}/second"))
        .await
        .unwrap();
    let back = page.back().await.unwrap();
    assert!(back.ok, "{:?}", back.error);
    assert!(
        back.snapshot.url.ends_with("/files"),
        "{}",
        back.snapshot.url
    );
    let fwd = page.forward().await.unwrap();
    assert!(fwd.ok, "{:?}", fwd.error);
    assert!(
        fwd.snapshot.url.ends_with("/second"),
        "{}",
        fwd.snapshot.url
    );
    // At the end of the history there is nothing further forward.
    let end = page.forward().await;
    assert!(end.is_err() || !end.unwrap().ok);

    // A reload loads the page again: what script set on the old document is gone.
    page.evaluate("window.__kept = 42").await.unwrap();
    let r = page.reload().await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(r.snapshot.url.ends_with("/second"), "{}", r.snapshot.url);
    let kept = page.evaluate("typeof window.__kept").await.unwrap();
    assert_eq!(kept.as_str(), Some("undefined"));
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn back_after_two_opens_on_the_first_tab_returns_the_page_it_went_to() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.first_page().await.unwrap();
    let inbox = tempfile::tempdir().unwrap();
    b.allow_downloads(DownloadPolicy::new(inbox.path().join("inbox")))
        .await
        .unwrap();
    let opts = SnapshotOptions::default();
    let dialogs = browser_cdp::DialogOptions::default();
    page.navigate_snapshot_with(&format!("http://127.0.0.1:{port}/files"), &opts, &dialogs)
        .await
        .unwrap();
    page.navigate_snapshot_with(&format!("http://127.0.0.1:{port}/second"), &opts, &dialogs)
        .await
        .unwrap();
    let back = page.back().await.unwrap();
    assert!(back.ok, "{:?}", back.error);
    assert!(
        back.snapshot.url.ends_with("/files"),
        "{}",
        back.snapshot.url
    );
    assert!(
        back.snapshot.render().contains("Pick"),
        "{}",
        back.snapshot.render()
    );
    let _ = b.close().await;
}
