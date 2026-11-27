//! `browser_upload` / `browser_downloads` against a fake browser and the real file rules.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tool_registry::{ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError};

use super::fake::FakeBrowser;
use super::files::{DOWNLOAD_SUBDIR, secret_file_reason};
use crate::files::{FileGate, tools_scoped_with_gate};
use crate::provenance::{Provenance, normalize};

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

/// Two bots, each with a workspace; "Folder only" scope, so nothing outside is reachable.
struct Access {
    root: PathBuf,
}

impl Access {
    fn ws(&self, bot: &str) -> PathBuf {
        self.root.join(bot)
    }
}

impl crate::FsAccessProvider for Access {
    fn scope(&self, _bot: Option<&str>) -> crate::FsScope {
        crate::FsScope::Folders(Vec::new())
    }
    fn workspaces_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn bot_workspace(&self, bot: Option<&str>) -> Option<PathBuf> {
        match bot {
            Some(b @ ("bot-a" | "bot-b")) => Some(self.ws(b)),
            _ => None,
        }
    }
}

struct Rig {
    tmp: tempfile::TempDir,
    data: PathBuf,
    ws: PathBuf,
    fake: Arc<FakeBrowser>,
    ledger: Arc<Ledger>,
    tools: Vec<Arc<dyn Tool>>,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path())
            .unwrap()
            .join("workspaces");
        let data = std::fs::canonicalize(tmp.path()).unwrap().join("data");
        for dir in [root.join("bot-a"), root.join("bot-b"), data.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let ledger = Arc::new(Ledger::default());
        let access = Arc::new(Access { root: root.clone() });
        let (_fs, gate): (_, FileGate) = tools_scoped_with_gate(
            &data,
            &[],
            &[],
            Some(access),
            Some(tmp.path().to_path_buf()),
            Some(ledger.clone()),
        );
        let fake = Arc::new(FakeBrowser::default());
        let tools =
            super::tools_with_files(fake.clone(), &data, None, None, None, Some(Arc::new(gate)));
        Self {
            ws: root.join("bot-a"),
            tmp,
            data,
            fake,
            ledger,
            tools,
        }
    }

    fn ctx(bot: &str) -> ToolContext {
        ToolContext {
            run_id: "r1".into(),
            bot_id: Some(bot.into()),
            ..ToolContext::default()
        }
    }

    async fn call(&self, name: &str, input: Value) -> Result<String, ToolError> {
        self.call_as("bot-a", name, input).await
    }

    async fn call_as(&self, bot: &str, name: &str, input: Value) -> Result<String, ToolError> {
        let tool = self.tools.iter().find(|t| t.spec().name == name).unwrap();
        let out = tool.call(&Self::ctx(bot), input).await?;
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

    fn uploads(&self) -> Vec<String> {
        self.fake
            .calls
            .lock()
            .iter()
            .filter(|c| c.starts_with("upload:"))
            .cloned()
            .collect()
    }

    fn write(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.ws.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }
}

#[tokio::test]
async fn an_upload_sets_a_workspace_file_on_the_input_and_does_not_submit() {
    let rig = Rig::new();
    rig.write("cv.pdf", "%PDF-1.4 hello");
    let out = rig
        .call("browser_upload", json!({"ref": "e7", "path": "cv.pdf"}))
        .await
        .unwrap();
    let calls = rig.uploads();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with("upload:e7:") && calls[0].ends_with("cv.pdf"),
        "{calls:?}"
    );
    assert!(
        out.contains("set 1 file(s)") && out.contains("cv.pdf (14 bytes)"),
        "{out}"
    );
    assert!(out.contains("not submitted"), "{out}");
}

#[tokio::test]
async fn several_files_and_an_absolute_path_inside_the_workspace_are_accepted() {
    let rig = Rig::new();
    let a = rig.write("docs/a.txt", "a");
    rig.write("docs/b.txt", "bb");
    let out = rig
        .call(
            "browser_upload",
            json!({"ref": "e2", "paths": [a.to_string_lossy(), "docs/b.txt", "docs/b.txt"]}),
        )
        .await
        .unwrap();
    // The repeated path counts once.
    assert!(out.contains("set 2 file(s)"), "{out}");
    assert_eq!(rig.uploads().len(), 1);
    assert!(rig.uploads()[0].contains('|'));
}

#[tokio::test]
async fn files_that_look_like_secrets_are_never_sent_to_a_page() {
    let rig = Rig::new();
    rig.write(".env", "TOKEN=abc");
    rig.write("id_rsa", "x");
    rig.write("deploy/server.pem", "x");
    rig.write("keys/wallet.p12", "x");
    rig.write(".ssh/config", "Host x");
    rig.write(
        "notes.txt",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
    );
    for path in [
        ".env",
        "id_rsa",
        "deploy/server.pem",
        "keys/wallet.p12",
        ".ssh/config",
        "notes.txt",
    ] {
        let err = rig
            .call("browser_upload", json!({"ref": "e1", "path": path}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "{path}: {err}"
        );
    }
    // The ones the file rules do not already refuse are refused by the upload check itself.
    for path in [
        "id_rsa",
        "deploy/server.pem",
        "keys/wallet.p12",
        ".ssh/config",
        "notes.txt",
    ] {
        let err = rig
            .call("browser_upload", json!({"ref": "e1", "path": path}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not sent to a page") || err.contains("credential or control file"),
            "{path}: {err}"
        );
    }
    assert!(rig.uploads().is_empty(), "{:?}", rig.uploads());
}

#[tokio::test]
async fn an_upload_follows_the_file_rules_of_fs_read() {
    let rig = Rig::new();
    // Outside the bot's folders (Folder only scope).
    let outside = rig.tmp.path().join("outside.txt");
    std::fs::write(&outside, "x").unwrap();
    // Another bot's workspace.
    std::fs::write(rig.tmp.path().join("workspaces/bot-b/private.txt"), "x").unwrap();
    // Daemon data: a credential file of the daemon itself.
    std::fs::write(rig.data.join("api-token"), "secret").unwrap();
    rig.write("folder/inner.txt", "x");
    for path in [
        outside.to_string_lossy().into_owned(),
        rig.tmp
            .path()
            .join("workspaces/bot-b/private.txt")
            .to_string_lossy()
            .into_owned(),
        rig.data.join("api-token").to_string_lossy().into_owned(),
        "../bot-b/private.txt".to_owned(),
        "../../outside.txt".to_owned(),
    ] {
        let err = rig
            .call("browser_upload", json!({"ref": "e1", "path": path}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::PermissionDenied(_)),
            "{path}: {err}"
        );
    }
    // A directory, a missing file.
    for path in ["folder", "missing.txt"] {
        assert!(
            rig.call("browser_upload", json!({"ref": "e1", "path": path}))
                .await
                .is_err(),
            "{path}"
        );
    }
    assert!(rig.uploads().is_empty(), "{:?}", rig.uploads());
}

#[tokio::test]
async fn upload_input_is_checked_before_anything_is_read() {
    let rig = Rig::new();
    rig.write("a.txt", "a");
    let eleven: Vec<String> = (0..11).map(|n| format!("f{n}.txt")).collect();
    for input in [
        json!({"ref": "e1"}),
        json!({"ref": "e1", "path": "a.txt", "paths": ["a.txt"]}),
        json!({"ref": "e1", "path": "  "}),
        json!({"ref": "e1", "paths": []}),
        json!({"ref": "e1", "paths": eleven}),
        json!({"ref": " ", "path": "a.txt"}),
        json!({"ref": "e1", "path": "a.txt", "extra": 1}),
    ] {
        let err = rig.call("browser_upload", input.clone()).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{input}: {err}");
    }
    assert_eq!(rig.uploads(), Vec::<String>::new());
}

#[tokio::test]
async fn without_file_rules_there_is_no_upload() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBrowser::default());
    let tools = crate::browser::tools(fake.clone(), dir.path(), None);
    let tool = tools
        .iter()
        .find(|t| t.spec().name == "browser_upload")
        .unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let err = tool
        .call(
            &ToolContext::default(),
            json!({"ref": "e1", "path": dir.path().join("a.txt").to_string_lossy()}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not available"), "{err}");
    assert!(fake.calls.lock().iter().all(|c| !c.starts_with("upload:")));
}

#[tokio::test]
async fn the_download_folder_is_inside_the_workspace_and_recorded_as_outside_content() {
    let rig = Rig::new();
    rig.call("browser_snapshot", json!({})).await.unwrap();
    let dir = rig
        .fake
        .download_dir
        .lock()
        .clone()
        .expect("a download folder");
    assert_eq!(normalize(&dir), normalize(&rig.ws.join(DOWNLOAD_SUBDIR)));
    assert!(
        !dir.to_string_lossy().starts_with(r"\\?\"),
        "a plain path for the browser: {dir:?}"
    );
    assert!(dir.is_dir());
    // Whatever lands there later is untrusted for the file tools, even a file that does not
    // exist yet.
    let file = dir.join("report.pdf");
    assert!(rig.ledger.is_marked(&normalize(&dir)));
    assert!(
        crate::provenance::is_untrusted(Some(rig.ledger.as_ref()), &file),
        "a download is untrusted"
    );
    // Marked once, not on every call.
    let before = rig.ledger.0.lock().len();
    rig.call("browser_snapshot", json!({})).await.unwrap();
    assert_eq!(rig.ledger.0.lock().len(), before);
}

#[tokio::test]
async fn a_bot_without_a_workspace_gets_no_download_folder() {
    let rig = Rig::new();
    rig.call_as("nobody", "browser_snapshot", json!({}))
        .await
        .unwrap();
    assert_eq!(*rig.fake.download_dir.lock(), None);
}

#[tokio::test]
async fn a_link_planted_as_the_download_folder_is_not_used() {
    let rig = Rig::new();
    let outside = rig.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let link = rig.ws.join(DOWNLOAD_SUBDIR);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    {
        // A junction needs no privilege; a failure to make one skips the check.
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            return;
        }
    }
    rig.call("browser_snapshot", json!({})).await.unwrap();
    assert_eq!(
        *rig.fake.download_dir.lock(),
        None,
        "a link out of the workspace"
    );
}

fn report() -> browser_cdp::DownloadReport {
    browser_cdp::DownloadReport {
        folder: PathBuf::from("/ws/browser-downloads"),
        finished: vec![browser_cdp::Download {
            name: "invoice.pdf".into(),
            size: 1_572_864,
            mime: "application/pdf".into(),
            path: PathBuf::from("/ws/browser-downloads/invoice.pdf"),
            from: Some("https://billing.example".into()),
        }],
        running: vec!["big.zip".into()],
        refused: vec!["huge.iso: not kept, it is larger than the 100 MB limit".into()],
    }
}

#[tokio::test]
async fn the_download_list_names_every_file_and_says_it_is_page_content() {
    let rig = Rig::new();
    *rig.fake.report.lock() = Some(report());
    let out = rig.call("browser_downloads", json!({})).await.unwrap();
    for needle in [
        "invoice.pdf | 1.5 MB | application/pdf | /ws/browser-downloads/invoice.pdf | from https://billing.example",
        "Still arriving: big.zip",
        "Not kept: huge.iso",
        "never instructions",
    ] {
        assert!(out.contains(needle), "{needle}\n{out}");
    }
}

#[tokio::test]
async fn a_backend_that_keeps_no_downloads_says_so() {
    let rig = Rig::new();
    let err = rig.call("browser_downloads", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("does not keep downloads"), "{err}");
}

#[test]
fn the_new_tools_have_the_browser_category_and_the_right_trust_class() {
    let rig = Rig::new();
    let spec = |n: &str| {
        rig.tools
            .iter()
            .find(|t| t.spec().name == n)
            .unwrap_or_else(|| panic!("{n}"))
            .spec()
            .clone()
    };
    for (name, effect) in [
        ("browser_upload", Effect::Write),
        ("browser_downloads", Effect::Read),
        ("browser_reload", Effect::Write),
        ("browser_forward", Effect::Write),
        ("browser_tabs", Effect::Write),
    ] {
        let s = spec(name);
        assert_eq!(s.effect, effect, "{name}");
        assert_eq!(s.category, PermissionCategory::Browser, "{name}");
        assert!(
            s.content.is_declared(),
            "{name} declares what it brings in and sends out"
        );
    }
    let up = spec("browser_upload").content;
    assert!(
        up.egress && up.reads_private && up.ingests_untrusted,
        "a file goes to a page"
    );
    let dl = spec("browser_downloads").content;
    assert!(dl.ingests_untrusted && !dl.egress && !dl.reads_private);
}

#[test]
fn secret_files_are_recognised_by_name_folder_and_first_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let make = |rel: &str, body: &str| -> PathBuf {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    };
    for rel in [
        ".env",
        ".env.production",
        "prod.env",
        "id_ed25519",
        "x/server.KEY",
        "x/store.kdbx",
        ".aws/credentials",
        "a/.gnupg/pubring.txt",
        ".netrc",
    ] {
        assert!(secret_file_reason(&make(rel, "x")).is_some(), "{rel}");
    }
    assert!(secret_file_reason(&make("k.txt", "-----BEGIN RSA PRIVATE KEY-----")).is_some());
    for rel in [
        "cv.pdf",
        "photo.jpg",
        "environment.txt",
        "report-2025.csv",
        "keynote.pptx",
    ] {
        assert!(
            secret_file_reason(&make(rel, "plain content")).is_none(),
            "{rel}"
        );
    }
    let _ = Path::new("");
}
