// Adapted from vercel-labs/agent-browser (Apache-2.0): browser lifecycle (launch / attach / close).
//! Browser lifecycle: launch Chrome/Edge with an isolated profile, or attach to a running one.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::client::CdpClient;
use crate::error::{Error, Result};
use crate::launch::{self, LaunchOptions};
use crate::page::Page;

/// A browser tab (or other target) known to the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// Target id.
    pub target_id: String,
    /// Target type (`page`, `iframe`, `service_worker`, ...).
    pub kind: String,
    /// Title.
    pub title: String,
    /// URL.
    pub url: String,
}

/// A connection to a browser, launched by us or attached to.
pub struct Browser {
    client: Arc<CdpClient>,
    endpoint: String,
    child: Option<Child>,
    own_process_group: bool,
    /// Temporary profile directory we created and must delete.
    owned_profile: Option<PathBuf>,
}

impl Browser {
    /// Launch Chrome/Edge with a fresh isolated profile (unless `user_data_dir` is given).
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        let exe = match &opts.executable {
            Some(p) => p.clone(),
            None => launch::find_browser()?,
        };
        let (profile, owned) = if let Some(p) = &opts.user_data_dir {
            (p.clone(), None)
        } else {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let p =
                std::env::temp_dir().join(format!("praxi-browser-{}-{nanos}", std::process::id()));
            (p.clone(), Some(p))
        };
        tokio::fs::create_dir_all(&profile).await?;
        // A stale file from a previous run would point at a dead port.
        let _ = tokio::fs::remove_file(profile.join("DevToolsActivePort")).await;

        let mut cmd = Command::new(&exe);
        cmd.args(launch::build_args(&opts, &profile))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        let own_process_group = {
            // Put Chromium and all renderer/GPU/utility children in an isolated process group
            // so closing/idling can reap the entire tree safely.
            cmd.process_group(0);
            true
        };
        #[cfg(not(unix))]
        let own_process_group = false;

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Launch(format!("{}: {e}", exe.display())))?;

        let connected = async {
            let ws = launch::wait_for_endpoint(&profile, opts.startup_timeout, &mut child).await?;
            CdpClient::connect(&ws).await.map(|c| (c, ws))
        }
        .await;
        match connected {
            Ok((client, endpoint)) => Ok(Self {
                client,
                endpoint,
                child: Some(child),
                own_process_group,
                owned_profile: owned,
            }),
            Err(e) => {
                let pid = child.id();
                terminate_and_reap_process_tree(&mut child, pid, own_process_group).await;
                if let Some(p) = owned {
                    let _ = tokio::fs::remove_dir_all(p).await;
                }
                Err(e)
            }
        }
    }

    /// Attach to a running browser via `ws://...`, `http://host:port` or `host:port`
    /// (started with `--remote-debugging-port`).
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let ws = launch::resolve_endpoint(endpoint).await?;
        Ok(Self {
            client: CdpClient::connect(&ws).await?,
            endpoint: ws,
            child: None,
            own_process_group: false,
            owned_profile: None,
        })
    }

    /// Connect to a CDP endpoint while assuming ownership of an already-spawned child process tree.
    pub async fn connect_with_owned_child(
        endpoint: &str,
        child: Child,
        own_process_group: bool,
    ) -> Result<Self> {
        let ws = launch::resolve_endpoint(endpoint).await?;
        Ok(Self {
            client: CdpClient::connect(&ws).await?,
            endpoint: ws,
            child: Some(child),
            own_process_group,
            owned_profile: None,
        })
    }

    /// Browser-level websocket URL (usable with [`Browser::connect`] from another process).
    pub fn ws_url(&self) -> &str {
        &self.endpoint
    }

    /// The underlying CDP connection.
    pub fn client(&self) -> &Arc<CdpClient> {
        &self.client
    }

    /// True once the browser's CDP connection has closed.
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Stop all active live-view captures running over this browser's CDP connection.
    pub async fn stop_live_views(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.client.stop_active_views()).await;
    }

    /// `Browser.getVersion` result (product, protocol version, user agent).
    pub async fn version(&self) -> Result<Value> {
        self.client.send("Browser.getVersion", json!({})).await
    }

    /// All targets of any type.
    pub async fn targets(&self) -> Result<Vec<TargetInfo>> {
        let r = self.client.send("Target.getTargets", json!({})).await?;
        Ok(r["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| TargetInfo {
                target_id: t["targetId"].as_str().unwrap_or_default().into(),
                kind: t["type"].as_str().unwrap_or_default().into(),
                title: t["title"].as_str().unwrap_or_default().into(),
                url: t["url"].as_str().unwrap_or_default().into(),
            })
            .collect())
    }

    /// Open tabs (targets of type `page`).
    pub async fn pages(&self) -> Result<Vec<TargetInfo>> {
        Ok(self
            .targets()
            .await?
            .into_iter()
            .filter(|t| t.kind == "page")
            .collect())
    }

    /// Open a new tab. Navigates to `url` when it is not `about:blank`.
    pub async fn new_page(&self, url: &str) -> Result<Page> {
        let r = self
            .client
            .send("Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let id = r["targetId"]
            .as_str()
            .ok_or_else(|| Error::Json("createTarget returned no targetId".into()))?;
        let page = Page::attach(self.client.clone(), id).await?;
        if url != "about:blank" {
            page.navigate(url).await?;
        }
        Ok(page)
    }

    /// Attach to an existing tab by target id.
    pub async fn attach(&self, target_id: &str) -> Result<Page> {
        Page::attach(self.client.clone(), target_id).await
    }

    /// Attach to the first existing tab, or open one when there is none.
    pub async fn first_page(&self) -> Result<Page> {
        match self.pages().await?.first() {
            Some(t) => self.attach(&t.target_id).await,
            None => self.new_page("about:blank").await,
        }
    }

    /// Close the browser. Stops any active live-view captures first, closes the browser,
    /// terminates and reaps the owned process tree, and removes any temporary profile.
    pub async fn close(mut self) -> Result<()> {
        self.stop_live_views().await;
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            if !self.client.is_closed() {
                let _ = self
                    .client
                    .send_with_timeout(None, "Browser.close", json!({}), Duration::from_secs(3))
                    .await;
            }
            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
            {
                terminate_and_reap_process_tree(&mut child, pid, self.own_process_group).await;
            } else if self.own_process_group {
                terminate_and_reap_process_tree(&mut child, pid, true).await;
            }
        }
        if let Some(p) = self.owned_profile.take() {
            // The profile may be briefly locked while the process winds down.
            for _ in 0..10 {
                if tokio::fs::remove_dir_all(&p).await.is_ok() || !p.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Ok(())
    }
}

async fn terminate_and_reap_process_tree(
    child: &mut Child,
    pid: Option<u32>,
    own_process_group: bool,
) {
    let still_running = child.try_wait().ok().flatten().is_none();
    #[cfg(unix)]
    if own_process_group && let Some(pgid) = pid.filter(|p| *p > 1) {
        let mut kill_pg = Command::new("/bin/kill");
        kill_pg
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{pgid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(mut k) = kill_pg.spawn() {
            let _ = tokio::time::timeout(Duration::from_secs(2), k.wait()).await;
        }
    }
    #[cfg(windows)]
    if still_running && let Some(p) = pid.filter(|p| *p > 0) {
        let _ = own_process_group;
        let mut tk = Command::new("taskkill");
        tk.args(["/F", "/T", "/PID", &p.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(mut k) = tk.spawn() {
            let _ = tokio::time::timeout(Duration::from_secs(3), k.wait()).await;
        }
    }
    #[cfg(not(unix))]
    let _ = own_process_group;
    #[cfg(not(any(unix, windows)))]
    let _ = (still_running, pid, own_process_group);

    if still_running {
        let _ = child.kill().await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

fn terminate_process_tree_blocking(child: &mut Child, own_process_group: bool) {
    let pid = child.id();
    let still_running = child.try_wait().ok().flatten().is_none();
    #[cfg(unix)]
    if own_process_group && let Some(pgid) = pid.filter(|p| *p > 1) {
        let _ = std::process::Command::new("/bin/kill")
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{pgid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    if still_running && let Some(p) = pid.filter(|p| *p > 0) {
        let _ = own_process_group;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &p.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = own_process_group;
    #[cfg(not(any(unix, windows)))]
    let _ = (still_running, pid, own_process_group);

    if still_running {
        let _ = child.start_kill();
    }
    let _ = child.try_wait();
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_process_tree_blocking(&mut child, self.own_process_group);
        }
        // Clean any temporary profile directory on a best-effort basis.
        if let Some(p) = self.owned_profile.take() {
            std::thread::spawn(move || {
                for _ in 0..10 {
                    if std::fs::remove_dir_all(&p).is_ok() || !p.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
        }
    }
}
