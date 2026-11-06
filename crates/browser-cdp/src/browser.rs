// Adapted from vercel-labs/agent-browser (Apache-2.0): browser lifecycle (launch / attach / close).
//! Browser lifecycle: launch Chrome/Edge with an isolated profile, or attach to a running one.

use std::path::PathBuf;
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

        let mut child = Command::new(&exe)
            .args(launch::build_args(&opts, &profile))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
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
                owned_profile: owned,
            }),
            Err(e) => {
                let _ = child.kill().await;
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

    /// Close the browser. Launched browsers are terminated and their temporary profile removed;
    /// attached browsers are left running.
    pub async fn close(mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            let _ = self.client.send("Browser.close", json!({})).await;
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
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

impl Drop for Browser {
    fn drop(&mut self) {
        // `kill_on_drop` terminates the process; clean the profile on a best-effort basis.
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
