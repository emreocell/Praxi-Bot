//! Eval-style pack for everyday browser use. Hermetic: every page is served by
//! a throw-away server on 127.0.0.1 that only exists inside the test; nothing
//! touches the network. Needs an installed Chrome/Edge, like the other browser
//! tests: `cargo test -p browser-cdp --test eval_pack -- --ignored --test-threads=1`

use std::time::Duration;

use browser_cdp::{Browser, LaunchOptions, SnapshotOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const HOME: &str = "<title>Home</title><h1>Welcome to the shop</h1><p>Open daily from nine.</p>\
<a href=\"/about\">About us</a><a href=\"/popup\" target=\"_blank\">Open popup</a>\
<a href=\"/file.bin\">Get file</a>";
const ABOUT: &str = "<title>About</title><h1>About us</h1><p>Founded in 1999.</p>";
const POPUP: &str = "<title>Popup</title><h1>Second tab</h1>";
const FORM: &str = "<title>Form</title><form action=\"/thanks\" method=\"get\">\
<label>Name <input name=name type=text></label><button type=submit>Send</button></form>";
const THANKS: &str = "<title>Thanks</title><h1>Thanks</h1>";
// The image is never answered: `load` does not fire.
const STUCK: &str = "<title>Stuck</title><h1>Hello from a slow page</h1>\
<img src=\"/hold.png\" width=1 height=1><button>Ok</button>";

async fn serve() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut s, _)) = l.accept().await {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let path = target.split('?').next().unwrap_or("/");
            if path == "/hold.png" {
                held.push(s);
                continue;
            }
            let (ctype, extra, body): (&str, &str, &str) = match path {
                "/about" => ("text/html", "", ABOUT),
                "/popup" => ("text/html", "", POPUP),
                "/form" => ("text/html", "", FORM),
                "/thanks" => ("text/html", "", THANKS),
                "/stuck" => ("text/html", "", STUCK),
                "/file.bin" => (
                    "application/octet-stream",
                    "Content-Disposition: attachment; filename=\"x.bin\"\r\n",
                    "BINARY",
                ),
                _ => ("text/html", "", HOME),
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
        }
    });
    port
}

fn ref_of(snap: &browser_cdp::Snapshot, role: &str, name: &str) -> String {
    snap.nodes
        .iter()
        .find(|n| n.role == role && n.name == name)
        .and_then(|n| n.ref_id.clone())
        .unwrap_or_else(|| panic!("no {role} \"{name}\" in:\n{}", snap.render()))
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn open_a_page_and_read_its_text_then_follow_a_link() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.new_page("about:blank").await.unwrap();
    let r = page
        .navigate_snapshot(
            &format!("http://127.0.0.1:{port}/"),
            &SnapshotOptions::default(),
        )
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let text = r.snapshot.render();
    assert!(text.contains("Welcome to the shop"), "{text}");
    let link = ref_of(&r.snapshot, "link", "About us");
    let r = page.click(&link).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(r.snapshot.render().contains("Founded in 1999"));
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn fill_and_submit_a_form() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b
        .new_page(&format!("http://127.0.0.1:{port}/form"))
        .await
        .unwrap();
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let name = ref_of(&snap, "textbox", "Name");
    page.fill(&name, "Emre").await.unwrap();
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let send = ref_of(&snap, "button", "Send");
    let r = page.click(&send).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(
        r.snapshot.render().contains("Thanks"),
        "{}",
        r.snapshot.render()
    );
    assert!(r.snapshot.url.contains("name=Emre"), "{}", r.snapshot.url);
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_link_that_opens_a_new_tab_is_listed_as_a_second_tab() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b
        .new_page(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let before = b.pages().await.unwrap().len();
    page.click(&ref_of(&snap, "link", "Open popup"))
        .await
        .unwrap();
    let mut tabs = Vec::new();
    for _ in 0..30 {
        tabs = b.pages().await.unwrap();
        if tabs.len() > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(tabs.len() > before, "{tabs:?}");
    assert!(tabs.iter().any(|t| t.url.ends_with("/popup")), "{tabs:?}");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_download_is_cancelled_and_never_written() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b
        .new_page(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let mut events = b.client().subscribe();
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let _ = page.click(&ref_of(&snap, "link", "Get file")).await;
    let state = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = events.recv().await.unwrap();
            if ev.method == "Browser.downloadProgress"
                && ev.params["state"].as_str() != Some("inProgress")
            {
                return ev.params["state"].as_str().unwrap_or_default().to_owned();
            }
        }
    })
    .await
    .expect("a download event");
    assert_eq!(state, "canceled");
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_page_that_never_finishes_loading_is_still_read_with_a_note() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.new_page("about:blank").await.unwrap();
    let started = std::time::Instant::now();
    let r = page
        .navigate_snapshot(
            &format!("http://127.0.0.1:{port}/stuck"),
            &SnapshotOptions::default(),
        )
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(r.snapshot.render().contains("Hello from a slow page"));
    let note = r.settle_note.expect("a still-loading note");
    assert!(note.contains("still loading"), "{note}");
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "took {:?}",
        started.elapsed()
    );
    let _ = b.close().await;
}
