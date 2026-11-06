//! Launch headless Chrome/Edge, open example.com, print the snapshot, click the link, print the diff.
//!
//! Run: `cargo run -p browser-cdp --example demo`

use browser_cdp::{Browser, LaunchOptions, SnapshotOptions};

#[tokio::main]
async fn main() -> browser_cdp::Result<()> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    println!("browser: {}", browser.version().await?["product"]);

    let page = browser.new_page("https://example.com").await?;
    let opts = SnapshotOptions {
        compact: true,
        ..SnapshotOptions::default()
    };
    let snap = page.snapshot(&opts).await?;
    println!("--- snapshot ---\n{}\n", snap.render());

    let link = snap
        .nodes
        .iter()
        .find(|n| n.role == "link")
        .and_then(|n| n.ref_id.clone())
        .expect("example.com has a link");
    println!("--- click {link} ---");
    let res = page.click(&link).await?;
    println!("ok: {}", res.ok);
    println!("{}", res.new_snapshot_diff.render());

    browser.close().await
}
