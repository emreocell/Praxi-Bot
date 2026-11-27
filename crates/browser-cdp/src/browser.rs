// Adapted from vercel-labs/agent-browser (Apache-2.0): browser lifecycle (launch / attach / close).
//! Browser lifecycle: launch Chrome/Edge with an isolated profile, or attach to a running one.

use std::collections::HashSet;
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
    /// The filtering proxy of a served session (stops with the browser).
    _proxy: Option<crate::proxy::FilterProxy>,
    /// Tabs we own; set only on a launched, unserved browser (popup guard on).
    guarded: Option<OwnedTargets>,
    /// The window belongs on a virtual screen and is kept maximised (every
    /// launch, every new tab or window).
    maximize: bool,
    /// Profile directory of a browser we launched: marked as cleanly closed
    /// once the process is gone, so the next start shows no "Restore pages".
    profile_dir: Option<PathBuf>,
    /// The folder downloads may land in, once [`Browser::allow_downloads`] was called.
    downloads: std::sync::Mutex<Option<Arc<crate::downloads::Downloads>>>,
}

/// The `Browser.setWindowBounds` parameters that maximise the window described
/// by a `Browser.getWindowForTarget` result, or `None` when it already is.
fn maximize_params(window: &Value) -> Option<Value> {
    let id = window["windowId"].as_i64()?;
    let state = window["bounds"]["windowState"].as_str().unwrap_or("normal");
    if matches!(state, "maximized" | "fullscreen") {
        return None;
    }
    Some(json!({"windowId": id, "bounds": {"windowState": "maximized"}}))
}

/// Ask the browser to maximise the window that shows `target_id`. A window
/// manager or a Chrome that refuses is only logged: the launch size is the
/// fallback.
async fn maximize_window_of(client: &CdpClient, target_id: &str) {
    let Ok(window) = client
        .send("Browser.getWindowForTarget", json!({"targetId": target_id}))
        .await
    else {
        return;
    };
    if let Some(params) = maximize_params(&window)
        && let Err(e) = client.send("Browser.setWindowBounds", params).await
    {
        tracing::debug!(error = %e, "could not maximise the browser window");
    }
}

/// Target ids this process created or found at start (the managed tabs).
type OwnedTargets = Arc<std::sync::Mutex<HashSet<String>>>;

fn owned_lock(o: &OwnedTargets) -> std::sync::MutexGuard<'_, HashSet<String>> {
    o.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Close every page we did not create that shows anything other than a blank
/// or public web page. `--disable-popup-blocking` lets pages open tabs, and a
/// `noopener` popup or `target=_blank` link has no opener, so ownership (not
/// the opener) decides. Residual: the browser has started that first request
/// when the target reports its address, so one request may leave before the
/// tab is closed.
async fn spawn_popup_guard(client: &Arc<CdpClient>, owned: &OwnedTargets) {
    let mut rx = client.subscribe();
    if client
        .send("Target.setDiscoverTargets", json!({"discover": true}))
        .await
        .is_err()
    {
        tracing::warn!("could not watch popups opened by pages");
        return;
    }
    // Tabs present now (the browser's first blank tab) belong to us.
    if let Ok(r) = client.send("Target.getTargets", json!({})).await {
        let mut set = owned_lock(owned);
        for t in r["targetInfos"].as_array().into_iter().flatten() {
            if t["type"].as_str() == Some("page")
                && let Some(id) = t["targetId"].as_str()
            {
                set.insert(id.to_owned());
            }
        }
    }
    let weak = Arc::downgrade(client);
    let owned = Arc::clone(owned);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let id = popup_to_close(&ev.method, &ev.params, &owned_lock(&owned));
                    if let Some(id) = id {
                        let Some(c) = weak.upgrade() else { break };
                        let _ = c.send("Target.closeTarget", json!({"targetId": id})).await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Events were dropped; one of them may have been a private tab.
                    let Some(c) = weak.upgrade() else { break };
                    reconcile_targets(&c, &owned).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Ask the browser for every target and close the strays.
async fn reconcile_targets(client: &Arc<CdpClient>, owned: &OwnedTargets) {
    let Ok(r) = client.send("Target.getTargets", json!({})).await else {
        return;
    };
    let strays = strays_to_close(&r, &owned_lock(owned));
    for id in strays {
        let _ = client
            .send("Target.closeTarget", json!({"targetId": id}))
            .await;
    }
}

/// Page targets in a `Target.getTargets` result that we do not own and that
/// show a non-public address.
fn strays_to_close(result: &Value, owned: &HashSet<String>) -> Vec<String> {
    result["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|info| stray_id(info, owned))
        .collect()
}

fn stray_id(info: &Value, owned: &HashSet<String>) -> Option<String> {
    let id = info["targetId"].as_str()?;
    if info["type"].as_str() != Some("page")
        || owned.contains(id)
        || crate::web_url::popup_url_allowed(info["url"].as_str().unwrap_or_default())
    {
        return None;
    }
    Some(id.to_owned())
}

/// The target to close for one browser event, if it is a page we do not own
/// that now shows a non-public address.
fn popup_to_close(method: &str, params: &Value, owned: &HashSet<String>) -> Option<String> {
    if !matches!(method, "Target.targetCreated" | "Target.targetInfoChanged") {
        return None;
    }
    stray_id(&params["targetInfo"], owned)
}

impl Browser {
    /// Launch Chrome/Edge with a fresh isolated profile (unless `user_data_dir` is given).
    pub async fn launch(mut opts: LaunchOptions) -> Result<Self> {
        launch::check_extra_args(&opts.args)?;
        // A served session: every request goes through our filtering proxy,
        // started before the browser so nothing ever leaves without it.
        let proxy = if let Some(net) = &opts.served {
            let p = crate::proxy::FilterProxy::start(net.private_host.clone())
                .await
                .map_err(|e| Error::Launch(format!("network guard: {e}")))?;
            opts.served_proxy = Some(p.addr());
            Some(p)
        } else {
            opts.served_proxy = None;
            None
        };
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
        // The last run may have been stopped abruptly: say it ended cleanly so
        // Chrome does not offer to "Restore pages" over the first page.
        {
            let dir = profile.clone();
            let _ =
                tokio::task::spawn_blocking(move || crate::profile::mark_clean_exit(&dir)).await;
        }
        if opts.user_data_dir.is_some() {
            let dir = profile.clone();
            tokio::task::spawn_blocking(move || {
                crate::saved_sites::prepare(&dir)?;
                let _ = crate::saved_sites::encrypt_directory(&dir);
                Ok::<_, Error>(())
            })
            .await
            .map_err(|e| Error::Io(e.to_string()))??;
        }
        // A stale file from a previous run would point at a dead port.
        let _ = tokio::fs::remove_file(profile.join("DevToolsActivePort")).await;

        let mut cmd = Command::new(&exe);
        cmd.args(launch::build_args(&opts, &profile))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // A service has no time zone in its environment; the browser should
        // see the host's, as any program the owner starts there would.
        #[cfg(target_os = "linux")]
        if let Some(tz) = crate::host::host_timezone() {
            cmd.env("TZ", tz);
        }
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

        // Keep the start and the end of the browser's stderr so a dead start says
        // why. The reader also drains the pipe for the browser's whole life.
        let stderr_tail = Arc::new(std::sync::Mutex::new(launch::StderrKeep::default()));
        if let Some(mut err) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 1024];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    tail.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(&buf[..n]);
                }
            });
        }
        let connected = async {
            let ws = launch::wait_for_endpoint(&profile, opts.startup_timeout, &mut child).await?;
            CdpClient::connect(&ws).await.map(|c| (c, ws))
        }
        .await;
        match connected {
            Ok((client, endpoint)) => {
                if let Some(net) = &opts.served {
                    client.set_destination_check(net.check.clone());
                }
                let guarded = if opts.served.is_none() {
                    let owned = OwnedTargets::default();
                    spawn_popup_guard(&client, &owned).await;
                    Some(owned)
                } else {
                    None
                };
                // A page must not be able to put a file on the owner's disk:
                // every download is cancelled (the events stay on for tests
                // and diagnostics). Chromium before this command existed
                // never reaches here, so a failure is only logged.
                if let Err(e) = client
                    .send(
                        "Browser.setDownloadBehavior",
                        json!({"behavior": "deny", "eventsEnabled": true}),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "could not switch browser downloads off");
                }
                let maximize = launch::fills_screen(
                    &opts,
                    cfg!(target_os = "linux"),
                    crate::host::has_display(),
                );
                if maximize
                    && let Ok(r) = client.send("Target.getTargets", json!({})).await
                    && let Some(id) = r["targetInfos"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|t| t["type"].as_str() == Some("page"))
                        .and_then(|t| t["targetId"].as_str())
                {
                    maximize_window_of(&client, id).await;
                }
                Ok(Self {
                    client,
                    endpoint,
                    child: Some(child),
                    own_process_group,
                    owned_profile: owned,
                    _proxy: proxy,
                    guarded,
                    maximize,
                    profile_dir: Some(profile),
                    downloads: std::sync::Mutex::default(),
                })
            }
            Err(e) => {
                let pid = child.id();
                terminate_and_reap_process_tree(&mut child, pid, own_process_group).await;
                if let Some(p) = owned {
                    let _ = tokio::fs::remove_dir_all(p).await;
                }
                // Give the reader a moment to take the last bytes the dying browser wrote.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let said = {
                    let t = stderr_tail
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    launch::stderr_summary(&t.text())
                };
                match e {
                    Error::Launch(m) if !said.is_empty() => {
                        Err(Error::Launch(format!("{m}: {said}")))
                    }
                    other => Err(other),
                }
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
            _proxy: None,
            guarded: None,
            maximize: false,
            profile_dir: None,
            downloads: std::sync::Mutex::default(),
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
            _proxy: None,
            guarded: None,
            maximize: false,
            profile_dir: None,
            downloads: std::sync::Mutex::default(),
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
        if let Some(owned) = &self.guarded {
            reconcile_targets(&self.client, owned).await;
        }
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
        if let Some(owned) = &self.guarded {
            owned_lock(owned).insert(id.to_owned());
        }
        if self.maximize {
            maximize_window_of(&self.client, id).await;
        }
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

    /// Close the tab `target_id`. Closing the last tab is the caller's call to make (a browser
    /// with no tab left shows nothing to read).
    pub async fn close_page(&self, target_id: &str) -> Result<()> {
        self.client
            .send("Target.closeTarget", json!({"targetId": target_id}))
            .await?;
        if let Some(owned) = &self.guarded {
            owned_lock(owned).remove(target_id);
        }
        Ok(())
    }

    /// Let this browser keep downloads, in `policy.dir` only (see [`crate::downloads`]). Until
    /// this is called every download is refused. Calling it again with the same folder changes
    /// nothing; with another folder the new one replaces the old.
    ///
    /// Never call this on a browser the owner started themselves: it changes how THEIR
    /// browser treats downloads.
    pub async fn allow_downloads(&self, policy: crate::downloads::DownloadPolicy) -> Result<()> {
        let existing = self
            .downloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if existing
            .as_ref()
            .is_some_and(|d| d.policy() == &crate::downloads::resolved(&policy))
        {
            return Ok(());
        }
        let fresh = crate::downloads::enable(&self.client, policy).await?;
        let old = self
            .downloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(fresh);
        if let Some(old) = old {
            old.retire();
        }
        Ok(())
    }

    /// The download folder's content and what this session knows about downloads that did not
    /// finish; `None` while downloads are refused.
    pub fn downloads(&self) -> Option<crate::downloads::DownloadReport> {
        self.downloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|d| d.report())
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
            // Browser.close lets Chrome write its profile and say "exited
            // cleanly"; only a browser that ignores it is killed.
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                terminate_and_reap_process_tree(&mut child, pid, self.own_process_group).await;
            } else if self.own_process_group {
                terminate_and_reap_process_tree(&mut child, pid, true).await;
            }
        }
        // Whatever way it ended (a kill leaves "Crashed" behind), the next
        // start of this profile must not offer to restore pages.
        if let Some(dir) = self.profile_dir.take()
            && self.owned_profile.is_none()
        {
            let _ =
                tokio::task::spawn_blocking(move || crate::profile::mark_clean_exit(&dir)).await;
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

#[cfg(test)]
mod popup_guard_tests {
    use super::{popup_to_close, strays_to_close};
    use serde_json::json;
    use std::collections::HashSet;

    fn ev(url: &str, opener: Option<&str>, kind: &str) -> serde_json::Value {
        json!({"targetInfo": {"targetId": "T1", "type": kind, "url": url, "openerId": opener}})
    }

    #[test]
    fn a_page_we_do_not_own_on_a_private_address_is_closed_with_or_without_opener() {
        let owned = HashSet::new();
        for url in [
            "http://localhost:8080/admin",
            "file:///etc/passwd",
            "http://192.168.0.1/",
        ] {
            for opener in [Some("P"), None] {
                let e = ev(url, opener, "page");
                assert_eq!(
                    popup_to_close("Target.targetInfoChanged", &e, &owned).as_deref(),
                    Some("T1"),
                    "{url} {opener:?}"
                );
            }
        }
        let blank = ev("", None, "page");
        assert!(popup_to_close("Target.targetCreated", &blank, &owned).is_none());
        let public = ev("https://example.com/", None, "page");
        assert!(popup_to_close("Target.targetCreated", &public, &owned).is_none());
        let worker = ev("file:///x", Some("P"), "service_worker");
        assert!(popup_to_close("Target.targetCreated", &worker, &owned).is_none());
        // A tab we own was checked by the tool.
        let owned: HashSet<String> = ["T1".to_owned()].into();
        let mine = ev("file:///x", None, "page");
        assert!(popup_to_close("Target.targetCreated", &mine, &owned).is_none());
    }

    #[test]
    fn reconciliation_after_a_lag_finds_the_stray_private_tab() {
        let owned: HashSet<String> = ["mine".to_owned()].into();
        let r = json!({"targetInfos": [
            {"targetId": "mine", "type": "page", "url": "about:blank"},
            {"targetId": "pub", "type": "page", "url": "https://example.com/"},
            {"targetId": "stray", "type": "page", "url": "http://127.0.0.1:9/"},
            {"targetId": "sw", "type": "service_worker", "url": "file:///w.js"},
        ]});
        assert_eq!(strays_to_close(&r, &owned), vec!["stray".to_owned()]);
    }
}

#[cfg(test)]
mod window_tests {
    use super::maximize_params;
    use serde_json::json;

    #[test]
    fn a_window_that_is_not_maximised_is_asked_to_maximise() {
        let w = json!({"windowId": 7, "bounds": {"left": 10, "top": 10, "width": 1280, "height": 800, "windowState": "normal"}});
        assert_eq!(
            maximize_params(&w),
            Some(json!({"windowId": 7, "bounds": {"windowState": "maximized"}}))
        );
        // A result without a state is a normal window too.
        let w = json!({"windowId": 1, "bounds": {"width": 800}});
        assert!(maximize_params(&w).is_some());
        for state in ["maximized", "fullscreen"] {
            let w = json!({"windowId": 7, "bounds": {"windowState": state}});
            assert_eq!(maximize_params(&w), None, "{state}");
        }
        assert_eq!(maximize_params(&json!({})), None);
    }
}
