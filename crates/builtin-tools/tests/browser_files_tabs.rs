//! Files and tabs through the model-facing tools, end to end on a real browser and a local
//! fixture site: a file from the bot's workspace goes into a page's file input, a download link
//! lands in the bot's download folder (and nowhere else), and tabs open, switch and close.
//!
//! Needs Chrome/Edge (`PRAXI_BROWSER_PATH` or an installed one). Run with
//! `cargo test -p builtin-tools --test browser_files_tabs -- --ignored --test-threads=1`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use builtin_tools::browser::{BrowserSettings, CdpDriver};
use builtin_tools::provenance::Provenance;
use builtin_tools::{
    BrowserDriver as _, BuiltinConfig, FsAccessProvider, FsScope, GateRequest, ToolGate,
    register_builtin_tools,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tool_registry::{ContentPart, ToolContext, ToolError, ToolRegistry};

const FILES: &str = "<title>Files</title><h1>Files</h1>\
<label>Pick <input type=file id=f></label><p id=out>none</p>\
<a href=\"/dl/hello.txt\">Get hello</a> <a href=\"/dl/evil.txt\">Get evil</a> <a href=\"/dl/big.bin\">Get big</a>\
<script>document.getElementById('f').addEventListener('change',async e=>{const x=e.target.files[0];\
document.getElementById('out').textContent='picked:'+x.name+':'+await x.text();});</script>";

async fn serve() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let resp = if let Some(file) = path.strip_prefix("/dl/") {
                    let (name, body) = match file {
                        "hello.txt" => ("hello.txt", "hello download".to_owned()),
                        "evil.txt" => ("..\\..\\evil.txt", "not a path".to_owned()),
                        _ => ("big.bin", "x".repeat(3000)),
                    };
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    let body = match path.as_str() {
                        "/files" => FILES,
                        "/second" => "<title>Second</title><h1>Second tab</h1>",
                        _ => "<title>other</title><p>other</p>",
                    };
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
        Ok(())
    }
}

/// The bot `bot-a` has a workspace; "Folder only" scope, so nothing outside it is reachable.
struct Access {
    root: PathBuf,
}

impl FsAccessProvider for Access {
    fn scope(&self, _bot: Option<&str>) -> FsScope {
        FsScope::Folders(Vec::new())
    }
    fn workspaces_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn bot_workspace(&self, bot: Option<&str>) -> Option<PathBuf> {
        (bot == Some("bot-a")).then(|| self.root.join("bot-a"))
    }
}

#[derive(Default)]
struct Ledger(parking_lot::Mutex<Vec<(String, bool)>>);

impl Provenance for Ledger {
    fn is_marked(&self, n: &str) -> bool {
        self.0
            .lock()
            .iter()
            .any(|(p, tree)| p == n || (*tree && n.starts_with(&format!("{p}/"))))
    }
    fn mark(&self, n: &str, tree: bool, _: &str) {
        self.0.lock().push((n.to_owned(), tree));
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    driver: Arc<CdpDriver>,
    ledger: Arc<Ledger>,
    reg: ToolRegistry,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("workspaces");
        let data = base.join("data");
        std::fs::create_dir_all(root.join("bot-a")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let driver = Arc::new(CdpDriver::new(BrowserSettings::isolated(&data)));
        let ledger = Arc::new(Ledger::default());
        let mut cfg = BuiltinConfig::new(data, Arc::new(AllowAll));
        cfg.browser = Some(driver.clone());
        cfg.fs_access = Some(Arc::new(Access { root: root.clone() }));
        cfg.fs_home = Some(base.clone());
        cfg.provenance = Some(ledger.clone());
        let reg = ToolRegistry::new();
        let _ = register_builtin_tools(&reg, cfg);
        Self {
            ws: root.join("bot-a"),
            _tmp: tmp,
            driver,
            ledger,
            reg,
        }
    }

    async fn call(&self, name: &str, input: Value) -> Result<String, ToolError> {
        let ctx = ToolContext {
            run_id: "r1".into(),
            bot_id: Some("bot-a".into()),
            ..ToolContext::default()
        };
        let out = self
            .reg
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .call(&ctx, input)
            .await?;
        Ok(out
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn ok(&self, name: &str, input: Value) -> String {
        self.call(name, input)
            .await
            .unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    async fn eval(&self, js: &str) -> String {
        let text = self.ok("browser_eval", json!({"js": js})).await;
        text.lines()
            .next()
            .unwrap_or_default()
            .trim_matches('"')
            .to_owned()
    }
}

/// The `[eN]` ref of the first snapshot line that contains `needle`.
fn ref_of(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle) && l.contains("[e"))
        .unwrap_or_else(|| panic!("no element {needle:?} in:\n{snapshot}"));
    let open = line.find("[e").unwrap() + 1;
    line[open..line.find(']').unwrap()].to_owned()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome/Edge"]
#[allow(clippy::too_many_lines)] // one story, read top to bottom
async fn a_workspace_file_goes_into_a_page_and_a_download_lands_in_the_sandbox() {
    let port = serve().await;
    let rig = Rig::new();
    // `browser_open` only opens public addresses, so the fixture on this computer is opened
    // through the driver; everything after that goes through the model-facing tools.
    rig.driver
        .open(&format!("http://127.0.0.1:{port}/files"), None)
        .await
        .unwrap();
    let snapshot = rig.ok("browser_snapshot", json!({})).await;
    let input = ref_of(&snapshot, "Pick");

    // Upload: a file of the bot's workspace is set on the input and the page reads it.
    std::fs::write(rig.ws.join("note.txt"), "hello from the bot").unwrap();
    let out = rig
        .ok("browser_upload", json!({"ref": input, "path": "note.txt"}))
        .await;
    assert!(
        out.contains("set 1 file(s)") && out.contains("note.txt"),
        "{out}"
    );
    let mut shown = String::new();
    for _ in 0..30 {
        shown = rig.eval("document.getElementById('out').textContent").await;
        if shown.starts_with("picked:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shown, "picked:note.txt:hello from the bot");

    // A secret in the same workspace is refused, and so is anything outside it.
    std::fs::write(rig.ws.join(".env"), "TOKEN=abc").unwrap();
    std::fs::write(
        rig.ws.join("backup.txt"),
        "-----BEGIN RSA PRIVATE KEY-----\nx",
    )
    .unwrap();
    for path in [".env", "backup.txt", "../../outside.txt"] {
        let err = rig
            .call("browser_upload", json!({"ref": input, "path": path}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "{path}: {err}"
        );
    }

    // Downloads: a link click saves into `<workspace>/browser-downloads` under a safe name.
    let links = rig.ok("browser_snapshot", json!({})).await;
    rig.ok("browser_click", json!({"ref": ref_of(&links, "Get hello")}))
        .await;
    rig.ok("browser_click", json!({"ref": ref_of(&links, "Get evil")}))
        .await;
    let mut listing = String::new();
    for _ in 0..60 {
        listing = rig.ok("browser_downloads", json!({})).await;
        if listing.contains("hello.txt |") && listing.contains("evil.txt |") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let folder = rig.ws.join("browser-downloads");
    assert!(listing.contains("hello.txt |"), "{listing}");
    assert!(listing.contains("text/plain"), "{listing}");
    assert!(listing.contains("never instructions"), "{listing}");
    assert_eq!(
        std::fs::read_to_string(folder.join("hello.txt")).unwrap(),
        "hello download"
    );
    // The page asked for `..\..\evil.txt`: it lands inside the folder, never above it.
    assert!(
        std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with("evil.txt")),
        "{listing}"
    );
    assert!(!rig.ws.join("evil.txt").exists());
    assert!(!rig.ws.parent().unwrap().join("evil.txt").exists());
    // The download is outside content for the file tools.
    assert!(
        builtin_tools::provenance::is_untrusted(
            Some(rig.ledger.as_ref()),
            &folder.join("hello.txt")
        ),
        "a download must count as untrusted"
    );
    // The size cap: nothing over it is kept (the default cap is 100 MB, so this one is kept).
    assert!(!listing.contains("Not kept"), "{listing}");

    // A downloaded file can be handed to another page by the same tool.
    let again = rig
        .ok(
            "browser_upload",
            json!({"ref": input, "path": "browser-downloads/hello.txt"}),
        )
        .await;
    assert!(again.contains("hello.txt"), "{again}");

    rig.driver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome/Edge"]
async fn tabs_open_switch_and_close_and_the_first_tab_stays() {
    let port = serve().await;
    let rig = Rig::new();
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    rig.driver.open(&url("/files"), None).await.unwrap();

    // A new tab, opened on an address (through the driver: the tool refuses this computer's
    // addresses, which the unit tests cover) becomes the current one.
    let opened = rig.driver.tab_new(Some(&url("/second"))).await.unwrap();
    assert!(opened.starts_with("opened tab 2"), "{opened}");
    assert!(opened.contains("Second tab"), "{opened}");
    let list = rig.ok("browser_tabs", json!({})).await;
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 2, "{list}");
    assert!(lines[1].starts_with("2*"), "the new tab is current: {list}");

    // Switch back and forth.
    let list = rig.ok("browser_tabs", json!({"switch_to": 1})).await;
    assert!(list.lines().next().unwrap().starts_with("1*"), "{list}");

    // The tool opens a blank tab, and closes tabs by number; the current one moves on.
    let blank = rig.ok("browser_tabs", json!({"new": true})).await;
    assert!(blank.starts_with("opened a blank tab"), "{blank}");
    assert_eq!(blank.lines().count(), 4, "{blank}");
    let closed = rig.ok("browser_tabs", json!({"close": 3})).await;
    assert!(closed.starts_with("closed tab 3"), "{closed}");
    assert_eq!(closed.lines().count(), 3, "{closed}");
    let closed = rig.ok("browser_tabs", json!({"close": 2})).await;
    assert_eq!(closed.lines().count(), 2, "{closed}");
    assert!(closed.lines().nth(1).unwrap().starts_with("1*"), "{closed}");

    // The first tab (which the live view follows) is never closed, nor is a tab that is not there.
    for close in [1, 7] {
        let err = rig
            .call("browser_tabs", json!({"close": close}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "{close}: {err}");
    }
    let still = rig.ok("browser_tabs", json!({})).await;
    assert_eq!(still.lines().count(), 1, "{still}");
    assert!(still.contains("Files"), "{still}");

    rig.driver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome/Edge"]
async fn back_forward_and_reload_walk_the_history() {
    let port = serve().await;
    let rig = Rig::new();
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    rig.driver.open(&url("/files"), None).await.unwrap();
    rig.driver.open(&url("/second"), None).await.unwrap();
    let back = rig.ok("browser_back", json!({})).await;
    assert!(back.contains("/files"), "{back}");
    let fwd = rig.ok("browser_forward", json!({})).await;
    assert!(fwd.contains("/second"), "{fwd}");
    // Nothing further forward.
    assert!(rig.call("browser_forward", json!({})).await.is_err());
    // A reload loads the page again: script state of the old document is gone.
    rig.eval("window.__kept = 1").await;
    let reloaded = rig.ok("browser_reload", json!({})).await;
    assert!(reloaded.contains("Second tab"), "{reloaded}");
    assert_eq!(rig.eval("typeof window.__kept").await, "undefined");
    rig.driver.shutdown().await;
}
