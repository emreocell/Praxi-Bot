//! `desktop_open` over a scripted desktop and the file tools' real path rules: what it opens,
//! with which program, and what it refuses before anything starts.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError};

use crate::desktop::fake::FakeDesktop;
use crate::desktop::{DesktopSettings, tools_dynamic_with_files};
use crate::files::{FileGate, tools_scoped_with_gate};

/// One bot with a workspace; "Folder only" scope, so nothing outside is reachable.
struct Access {
    root: PathBuf,
}

impl crate::FsAccessProvider for Access {
    fn scope(&self, _bot: Option<&str>) -> crate::FsScope {
        crate::FsScope::Folders(Vec::new())
    }
    fn workspaces_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn bot_workspace(&self, bot: Option<&str>) -> Option<PathBuf> {
        (bot == Some("bot-a")).then(|| self.root.join("bot-a"))
    }
}

struct Rig {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    fake: Arc<FakeDesktop>,
    open: Arc<dyn Tool>,
}

impl Rig {
    fn new(platform: &'static str, with_files: bool) -> Self {
        Self::on(platform, with_files, true)
    }

    /// `local`: the desktop is the screen of the machine the daemon runs on.
    fn on(platform: &'static str, with_files: bool, local: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("workspaces");
        let data = base.join("data");
        for dir in [root.join("bot-a"), data.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(base.join("secret.html"), b"<p>outside</p>").unwrap();
        let (_fs, gate): (_, FileGate) = tools_scoped_with_gate(
            &data,
            &[],
            &[],
            Some(Arc::new(Access { root: root.clone() })),
            Some(base.clone()),
            None,
        );
        let fake = Arc::new(FakeDesktop {
            local_screen: local,
            ..FakeDesktop::new(false)
        });
        *fake.platform.lock() = platform;
        let tools = tools_dynamic_with_files(
            Some(fake.clone()),
            None,
            &data,
            DesktopSettings {
                action_settle: std::time::Duration::ZERO,
                launch_settle: std::time::Duration::ZERO,
            },
            None,
            None,
            with_files.then_some(gate),
        );
        let open = tools
            .into_iter()
            .find(|t| t.spec().name == "desktop_open")
            .expect("desktop_open is registered");
        Self {
            ws: root.join("bot-a"),
            tmp,
            fake,
            open,
        }
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.ws.join(name), body).unwrap();
    }

    async fn call(&self, input: Value) -> Result<String, ToolError> {
        let ctx = ToolContext {
            run_id: "r1".into(),
            bot_id: Some("bot-a".into()),
            ..ToolContext::default()
        };
        self.open
            .call(&ctx, input)
            .await
            .map(|o| match &o.content[0] {
                ContentPart::Text { text } => text.clone(),
                other => panic!("{other:?}"),
            })
    }

    fn started(&self) -> Vec<(String, Vec<String>)> {
        self.fake
            .launched
            .lock()
            .iter()
            .map(|a| (a.executable.clone(), a.args.clone()))
            .collect()
    }
}

#[tokio::test]
async fn a_page_of_the_workspace_opens_in_the_system_opener_or_in_the_named_browser() {
    let rig = Rig::new("linux", true);
    rig.write("kralinho.html", "<h1>Kralinho</h1>");
    let said = rig.call(json!({"path": "kralinho.html"})).await.unwrap();
    assert!(
        said.starts_with("opened kralinho.html with the default application"),
        "{said}"
    );
    let full = rig.ws.join("kralinho.html").to_string_lossy().into_owned();
    assert_eq!(rig.started(), [("xdg-open".to_owned(), vec![full.clone()])]);
    let said = rig
        .call(json!({"path": "kralinho.html", "with": "firefox"}))
        .await
        .unwrap();
    assert!(said.contains("with firefox"), "{said}");
    assert_eq!(rig.started()[1], ("firefox".to_owned(), vec![full]));
}

#[tokio::test]
async fn an_address_opens_and_a_browser_that_is_not_installed_falls_back_to_its_other_name() {
    let rig = Rig::new("linux", true);
    rig.fake.launch_fails.lock().push("google-chrome");
    rig.call(json!({"url": "https://example.com/a", "with": "chrome"}))
        .await
        .unwrap();
    let started = rig.started();
    assert_eq!(started.len(), 2, "{started:?}");
    assert_eq!(started[0].0, "google-chrome");
    assert_eq!(
        started[1],
        (
            "google-chrome-stable".into(),
            vec!["https://example.com/a".into()]
        )
    );
    rig.fake.launch_fails.lock().push("google-chrome-stable");
    let err = rig
        .call(json!({"url": "https://example.com/", "with": "chrome"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not installed"), "{err}");
}

#[tokio::test]
async fn what_is_outside_the_bots_folders_or_not_a_document_is_never_started() {
    let rig = Rig::new("linux", true);
    rig.write("run.sh", "echo hi");
    rig.write("tool.exe", "MZ");
    rig.write("page.html", "<p>x</p>");
    // a program or script, a path outside the workspace, a missing file, a folder, a traversal
    let outside = rig.tmp.path().join("secret.html");
    for bad in [
        json!({"path": "run.sh"}),
        json!({"path": "tool.exe"}),
        json!({"path": outside.to_string_lossy()}),
        json!({"path": "../../secret.html"}),
        json!({"path": "missing.html"}),
        json!({"path": "."}),
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "javascript:alert(1)"}),
        json!({"url": "https://user:pw@example.com/"}),
        json!({"path": "page.html", "with": "notepad"}),
        json!({"path": "page.html", "url": "https://example.com/"}),
    ] {
        assert!(rig.call(bad.clone()).await.is_err(), "{bad}");
    }
    assert!(rig.started().is_empty(), "nothing was started");
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_inside_the_workspace_that_leaves_it_is_not_followed() {
    let rig = Rig::new("linux", true);
    let target = rig.tmp.path().join("secret.html");
    std::os::unix::fs::symlink(&target, rig.ws.join("innocent.html")).unwrap();
    assert!(rig.call(json!({"path": "innocent.html"})).await.is_err());
    assert!(rig.started().is_empty(), "the link was not followed");
}

#[tokio::test]
async fn without_the_file_rules_or_on_another_computer_only_addresses_open() {
    let rig = Rig::new("linux", false);
    rig.write("page.html", "<p>x</p>");
    let err = rig
        .call(json!({"path": "page.html"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not available here"), "{err}");
    rig.call(json!({"url": "https://example.com/"}))
        .await
        .unwrap();
    assert_eq!(rig.started().len(), 1);

    // the file rules are there, but the desktop is another computer's (a paired one): a path
    // of THIS computer means nothing there, so only the address opens
    let remote = Rig::on("linux", true, false);
    remote.write("page.html", "<p>x</p>");
    let err = remote
        .call(json!({"path": "page.html"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("another computer"), "{err}");
    assert!(remote.started().is_empty(), "nothing was started");
    remote
        .call(json!({"url": "https://example.com/"}))
        .await
        .unwrap();
    assert_eq!(remote.started().len(), 1);
}

#[tokio::test]
async fn windows_and_mac_use_their_own_opener() {
    let win = Rig::new("windows", true);
    win.write("a.html", "x");
    win.call(json!({"url": "https://example.com/"}))
        .await
        .unwrap();
    win.call(json!({"url": "https://example.com/", "with": "edge"}))
        .await
        .unwrap();
    assert_eq!(
        win.started()[0],
        ("https://example.com/".to_owned(), vec![])
    );
    assert_eq!(win.started()[1].0, "msedge.exe");
    let mac = Rig::new("macos", true);
    mac.call(json!({"url": "https://example.com/"}))
        .await
        .unwrap();
    assert_eq!(mac.started()[0].0, "open");
    let other = Rig::new("fake", true);
    assert!(
        other
            .call(json!({"url": "https://example.com/"}))
            .await
            .is_err()
    );
}
