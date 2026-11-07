//! Integration tests that need a real desktop / Chrome / network.
//! Run with `cargo test -p builtin-tools --test integration -- --ignored --nocapture --test-threads=1`.

use std::sync::Arc;
#[cfg_attr(not(windows), allow(unused_imports))]
use std::time::{Duration, Instant};

use async_trait::async_trait;
use builtin_tools::{BuiltinConfig, GateRequest, ToolGate, register_builtin_tools};
use serde_json::{Value, json};
use tool_registry::{ContentPart, ToolContext, ToolRegistry};

struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
        Ok(())
    }
}

async fn call(reg: &ToolRegistry, name: &str, input: Value) -> String {
    let out = reg
        .get(name)
        .unwrap_or_else(|| panic!("{name} missing"))
        .call(&ToolContext::default(), input)
        .await
        .unwrap_or_else(|e| panic!("{name} failed: {e}"));
    match &out.content[0] {
        ContentPart::Text { text } => text.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a desktop session; opens and closes Notepad"]
async fn notepad_launch_type_by_index_state_shows_text() {
    let data = tempfile::tempdir().unwrap();
    let mut cfg = BuiltinConfig::new(data.path().to_path_buf(), Arc::new(AllowAll));
    cfg.desktop = Some(Arc::new(desktop_windows::WindowsBackend::new().unwrap()));
    let reg = ToolRegistry::new();
    let _ = register_builtin_tools(&reg, cfg);

    let before = call(&reg, "desktop_list_apps", json!({})).await;
    let known_ids: Vec<String> = before
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();

    println!(
        "before:
{before}"
    );
    let launched = call(&reg, "desktop_launch", json!({"executable": "notepad.exe"})).await;
    println!(
        "launch result:
{launched}"
    );
    let launched_pid: u32 = launched
        .split("pid=")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .expect("launched pid");

    // Find the new Notepad window (not one that was already open).
    let deadline = Instant::now() + Duration::from_secs(15);
    let id = loop {
        let list = call(&reg, "desktop_list_apps", json!({})).await;
        // Titles are localised ("Not Defteri"); the window id is new, and Notepad
        // may share one process with windows that were already open.
        let hit = list.lines().find(|l| {
            let t = l.to_lowercase();
            (t.contains("notepad") || t.contains("not defteri"))
                && !known_ids.contains(&l.split_whitespace().next().unwrap().to_string())
        });
        if let Some(l) = hit {
            let id = l
                .split_whitespace()
                .next()
                .unwrap()
                .trim_start_matches("id=");
            break id.parse::<u64>().unwrap();
        }
        assert!(Instant::now() < deadline, "no new Notepad window:\n{list}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    let state = call(&reg, "desktop_get_state", json!({"window_id": id})).await;
    println!("{state}");
    let editor = state
        .lines()
        .find(|l| l.starts_with('[') && (l.contains("] edit ") || l.contains("] document ")))
        .unwrap_or_else(|| panic!("no editor element in state:\n{state}"));
    let index: u32 = editor[1..editor.find(']').unwrap()].parse().unwrap();

    let after = call(
        &reg,
        "desktop_type",
        json!({"index": index, "text": "praxi-it-text"}),
    )
    .await;
    println!("{after}");
    let shows_text = after.contains("praxi-it-text");

    // Clean up: clear the editor, then close only the window we opened and the
    // launcher process we started (other Notepad windows are not ours).
    let _ = reg
        .get("desktop_key")
        .unwrap()
        .call(&ToolContext::default(), json!({"keys": ["ctrl", "a"]}))
        .await;
    let _ = reg
        .get("desktop_key")
        .unwrap()
        .call(&ToolContext::default(), json!({"keys": ["Delete"]}))
        .await;
    let _ = reg
        .get("desktop_key")
        .unwrap()
        .call(&ToolContext::default(), json!({"keys": ["alt", "F4"]}))
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let _ = call(
        &reg,
        "shell_run",
        json!({"command": format!("Stop-Process -Id {launched_pid} -Force -ErrorAction SilentlyContinue"), "shell": "powershell"}),
    )
    .await;

    assert!(
        shows_text,
        "state after typing does not contain the text:\n{after}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome and network access"]
async fn browser_open_example_com_and_click() {
    let data = tempfile::tempdir().unwrap();
    let cfg = BuiltinConfig::new(data.path().to_path_buf(), Arc::new(AllowAll));
    let driver = Arc::new(builtin_tools::CdpDriver::new(cfg.browser_settings.clone()));
    let mut cfg = cfg;
    cfg.browser = Some(driver.clone());
    let reg = ToolRegistry::new();
    let _ = register_builtin_tools(&reg, cfg);

    async {
        let page = call(&reg, "browser_open", json!({"url": "https://example.com"})).await;
        println!("{page}");
        assert!(page.contains("Example Domain"), "{page}");
        let link = page
            .lines()
            .find(|l| l.contains("link"))
            .unwrap_or_else(|| panic!("no link in snapshot:\n{page}"));
        let r = link[link.find("[e").unwrap() + 1..link.find(']').unwrap()].to_string();
        let after = call(&reg, "browser_click", json!({"ref": r})).await;
        println!("{after}");
        let title = call(
            &reg,
            "browser_eval",
            json!({"js": "document.location.href"}),
        )
        .await;
        println!("href after click: {title}");
        assert!(
            title.contains("iana.org"),
            "click did not navigate: {title}"
        );
    }
    .await;
    driver.shutdown().await;
}
