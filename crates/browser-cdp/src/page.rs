// Adapted from vercel-labs/agent-browser (Apache-2.0): frame discovery, AX + DOM capture, load waiting.
//! A single tab: session management, frames (including out-of-process iframes), snapshots.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast};
use tokio::task::JoinHandle;

use crate::client::{CdpClient, CdpEvent};
use crate::diff::{self, SnapshotDiff};
use crate::dom::{self, COMPUTED_STYLES};
use crate::error::{Error, Result};
use crate::live_view::{BrowserViewConfig, BrowserViewTarget};
use crate::snapshot::{
    DomExtra, FrameInput, RefEntry, RefMap, Snapshot, SnapshotOptions, build_snapshot,
};

/// Default navigation / wait timeout.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// Requests in flight longer than this (long-poll, `EventSource`) do not block network idle.
pub(crate) const INFLIGHT_CAP: Duration = Duration::from_secs(10);

/// A JavaScript dialog the page watcher answered on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogInfo {
    /// `alert`, `confirm`, `prompt`, `beforeunload`, or `unknown`.
    pub kind: String,
    /// The dialog message.
    pub message: String,
    /// True when it was accepted, false when dismissed.
    pub accepted: bool,
}

/// How to handle a JavaScript dialog that opens during an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogAction {
    /// Alerts are accepted; confirm, prompt, beforeunload, and unknown dialogs are dismissed.
    #[default]
    Auto,
    /// Explicitly accept dialogs (including confirm, prompt, beforeunload).
    Accept,
    /// Explicitly dismiss dialogs.
    Dismiss,
}

impl DialogAction {
    /// Parse from an optional `"accept"` | `"dismiss"` string; anything else defaults to Auto.
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("accept") => Self::Accept,
            Some("dismiss") => Self::Dismiss,
            _ => Self::Auto,
        }
    }
}

/// Options controlling JavaScript dialog handling during an action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogOptions {
    pub action: DialogAction,
    pub prompt_text: Option<String>,
}

impl DialogOptions {
    pub fn auto() -> Self {
        Self::default()
    }

    pub fn accept() -> Self {
        Self {
            action: DialogAction::Accept,
            prompt_text: None,
        }
    }

    pub fn dismiss() -> Self {
        Self {
            action: DialogAction::Dismiss,
            prompt_text: None,
        }
    }

    pub fn accept_prompt(text: impl Into<String>) -> Self {
        Self {
            action: DialogAction::Accept,
            prompt_text: Some(text.into()),
        }
    }
}

/// Policy: only `alert` is accepted under Auto; `confirm`/`prompt`/`beforeunload`/`unknown` are dismissed.
pub(crate) fn default_accepts_dialog(kind: &str) -> bool {
    matches!(kind, "alert")
}

/// Shared with the background event watcher.
#[derive(Default)]
pub(crate) struct Shared {
    /// Frame id -> flattened session of out-of-process iframes.
    pub oopif: StdMutex<HashMap<String, String>>,
    /// Sessions whose domains are already enabled.
    pub ready: StdMutex<HashSet<String>>,
    /// In-flight network requests of the main session, id -> first seen (a redirect chain reuses
    /// one id, so inserts are idempotent). Entries older than [`INFLIGHT_CAP`] are pruned.
    pub inflight: StdMutex<HashMap<String, Instant>>,
    /// JS dialogs handled by the watcher since the current action started.
    pub dialogs: StdMutex<Vec<DialogInfo>>,
    /// Policy for dialogs that open during the current action.
    pub dialog_policy: StdMutex<DialogOptions>,
    /// Number of AX/DOMSnapshot rounds taken (diagnostics, used by tests).
    pub snap_rounds: AtomicU64,
    /// Last network activity.
    pub last_net: StdMutex<Option<Instant>>,
    /// Incremented on every main-frame navigation; invalidates refs.
    pub nav_gen: AtomicU64,
    /// Set once this page/session detaches, crashes, or closes.
    pub target_closed: Arc<AtomicBool>,
    /// Signaled whenever `target_closed` transitions to `true`.
    pub target_close_notify: Arc<Notify>,
    /// Hooks notified on main-frame navigation or completed page actions.
    pub live_view_action_hooks: StdMutex<Vec<std::sync::Weak<dyn Fn() + Send + Sync>>>,
    /// Strong owner anchors removed if/when needed; callers hold their own `Arc` or register strong hooks.
    pub live_view_strong_hooks: StdMutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

/// Geometry and ownership of a frame, kept for coordinate translation.
#[derive(Debug, Clone)]
pub(crate) struct FrameMeta {
    pub id: String,
    pub parent: Option<String>,
    pub owner_backend: Option<i64>,
    pub session: Option<String>,
}

#[derive(Default)]
pub(crate) struct State {
    pub refs: RefMap,
    pub last: Option<Snapshot>,
    pub opts: SnapshotOptions,
    pub frames: Vec<FrameMeta>,
    pub gen_seen: u64,
}

/// One browser tab, attached over a flattened CDP session.
pub struct Page {
    pub(crate) client: Arc<CdpClient>,
    pub(crate) session: String,
    pub(crate) target_id: String,
    pub(crate) shared: Arc<Shared>,
    pub(crate) state: Mutex<State>,
    watcher: JoinHandle<()>,
}

impl Drop for Page {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl Page {
    /// Attach to an existing target and enable the CDP domains needed for snapshots and actions.
    pub async fn attach(client: Arc<CdpClient>, target_id: &str) -> Result<Self> {
        let rx = client.subscribe();
        let res = client
            .send(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session = res
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Json("attachToTarget returned no sessionId".into()))?
            .to_string();
        let shared = Arc::new(Shared::default());
        let watcher = tokio::spawn(watch(
            rx,
            session.clone(),
            target_id.to_string(),
            shared.clone(),
            Some(client.clone()),
        ));
        let page = Self {
            client,
            session,
            target_id: target_id.to_string(),
            shared,
            state: Mutex::new(State::default()),
            watcher,
        };
        page.enable_domains(&page.session.clone(), true).await?;
        Ok(page)
    }

    /// Underlying shared CDP websocket client.
    pub fn client(&self) -> &Arc<CdpClient> {
        &self.client
    }

    /// Target id of this tab.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// Flattened session id of this tab.
    pub fn session_id(&self) -> &str {
        &self.session
    }

    /// False once the CDP connection or this target session has closed/crashed.
    pub fn is_alive(&self) -> bool {
        !self.client.is_closed() && !self.shared.target_closed.load(Ordering::SeqCst)
    }

    /// Create a live-view adapter pinned to this tab.
    pub fn live_view(&self, config: BrowserViewConfig) -> Arc<BrowserViewTarget> {
        BrowserViewTarget::from_page(self, config)
    }

    pub(crate) fn live_view_close_signal(&self) -> (Arc<AtomicBool>, Arc<Notify>) {
        (
            self.shared.target_closed.clone(),
            self.shared.target_close_notify.clone(),
        )
    }

    pub(crate) fn register_live_view_action_hook(&self, hook: &Arc<dyn Fn() + Send + Sync>) {
        if let Ok(mut strong) = self.shared.live_view_strong_hooks.lock() {
            if strong.len() >= 16 {
                strong.remove(0);
            }
            strong.push(Arc::clone(hook));
        }
        if let Ok(mut hooks) = self.shared.live_view_action_hooks.lock() {
            hooks.retain(|w| w.strong_count() > 0);
            hooks.push(Arc::downgrade(hook));
        }
    }

    pub(crate) fn notify_live_view_action(&self) {
        notify_action_hooks(&self.shared);
    }

    /// Detach the session (the tab keeps running).
    pub async fn detach(&self) -> Result<()> {
        let res = self
            .client
            .send(
                "Target.detachFromTarget",
                json!({"sessionId": self.session}),
            )
            .await
            .map(|_| ());
        mark_target_closed(&self.shared);
        res
    }

    /// Close the tab.
    pub async fn close(&self) -> Result<()> {
        let res = self
            .client
            .send("Target.closeTarget", json!({"targetId": self.target_id}))
            .await
            .map(|_| ());
        mark_target_closed(&self.shared);
        res
    }

    /// Bring the tab to the foreground.
    pub async fn bring_to_front(&self) -> Result<()> {
        self.cmd("Page.bringToFront", json!({})).await.map(|_| ())
    }

    pub(crate) async fn enable_domains(&self, session: &str, main: bool) -> Result<()> {
        for d in [
            "Page.enable",
            "DOM.enable",
            "Accessibility.enable",
            "Runtime.enable",
        ] {
            self.client.send_on(Some(session), d, json!({})).await?;
        }
        if main {
            self.client
                .send_on(Some(session), "Network.enable", json!({}))
                .await?;
        }
        self.client
            .send_on(
                Some(session),
                "Target.setAutoAttach",
                json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            )
            .await?;
        if let Ok(mut r) = self.shared.ready.lock() {
            r.insert(session.to_string());
        }
        Ok(())
    }

    async fn ensure_ready(&self, session: &str) {
        let ready = self.shared.ready.lock().is_ok_and(|r| r.contains(session));
        if !ready {
            let _ = self.enable_domains(session, false).await;
        }
    }

    /// Send a command on the page's own session.
    pub(crate) async fn cmd(&self, method: &str, params: Value) -> Result<Value> {
        self.client
            .send_on(Some(&self.session), method, params)
            .await
    }

    /// Send a command on `session` (`None` = the page session).
    pub(crate) async fn cmd_on(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.client
            .send_on(Some(session.unwrap_or(&self.session)), method, params)
            .await
    }

    /// Evaluate JavaScript in the main frame and return the result as JSON.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let r = self
            .cmd(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("exception");
            return Err(Error::Js(msg.to_string()));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Current URL.
    pub async fn url(&self) -> Result<String> {
        Ok(self
            .evaluate("location.href")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Current title.
    pub async fn title(&self) -> Result<String> {
        Ok(self
            .evaluate("document.title")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    // -- frames ---------------------------------------------------------------------------------

    /// Walk the frame tree, hopping into out-of-process iframe sessions, and fetch AX data.
    async fn collect_frames(&self) -> Result<(Vec<FrameInput>, String)> {
        struct Job {
            session: Option<String>,
            parent: Option<String>,
            owner: Option<i64>,
        }
        self.shared.snap_rounds.fetch_add(1, Ordering::SeqCst);
        let mut main_url = String::new();
        let mut frames: Vec<FrameInput> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue = vec![Job {
            session: None,
            parent: None,
            owner: None,
        }];
        loop {
            while let Some(job) = queue.pop() {
                if let Some(s) = &job.session {
                    self.ensure_ready(s).await;
                }
                let tree = match self
                    .cmd_on(job.session.as_deref(), "Page.getFrameTree", json!({}))
                    .await
                {
                    Ok(t) => t,
                    // A child session that vanished mid-walk is skipped; the main one is fatal.
                    Err(e) if job.session.is_none() || e.is_fatal() => return Err(e),
                    Err(_) => continue,
                };
                if job.session.is_none() {
                    let f = &tree["frameTree"]["frame"];
                    main_url = format!(
                        "{}{}",
                        f["url"].as_str().unwrap_or_default(),
                        f["urlFragment"].as_str().unwrap_or_default()
                    );
                }
                // Depth-first walk of this session's tree: (node, is_session_root, parent, owner)
                let mut stack: Vec<(Value, bool, Option<String>, Option<i64>)> = vec![(
                    tree["frameTree"].clone(),
                    true,
                    job.parent.clone(),
                    job.owner,
                )];
                while let Some((node, root, parent, owner)) = stack.pop() {
                    let f = &node["frame"];
                    let id = f["id"].as_str().unwrap_or_default().to_string();
                    let child_session = (!root)
                        .then(|| self.shared.oopif.lock().ok()?.get(&id).cloned())
                        .flatten();
                    // Parent and owner of a session's root frame are resolved against the
                    // frames collected so far (the frame tree of the parent may not list it).
                    let (parent, owner) = if root && job.session.is_some() && parent.is_none() {
                        let pid = f["parentId"].as_str().map(str::to_string);
                        let psession = pid
                            .as_ref()
                            .and_then(|p| frames.iter().find(|x| &x.frame_id == p))
                            .and_then(|x| x.session_id.clone());
                        let owner = self.frame_owner(psession.as_deref(), &id).await;
                        (pid, owner)
                    } else if root {
                        (parent, owner)
                    } else {
                        (parent, self.frame_owner(job.session.as_deref(), &id).await)
                    };
                    if let Some(cs) = child_session {
                        if visited.insert(cs.clone()) {
                            queue.push(Job {
                                session: Some(cs),
                                parent: parent.clone(),
                                owner,
                            });
                        }
                        continue;
                    }
                    if let Some(s) = &job.session {
                        visited.insert(s.clone());
                    }
                    let ax_params = if root {
                        json!({})
                    } else {
                        json!({"frameId": id})
                    };
                    let ax = self
                        .cmd_on(
                            job.session.as_deref(),
                            "Accessibility.getFullAXTree",
                            ax_params,
                        )
                        .await
                        .map(|v| v["nodes"].as_array().cloned().unwrap_or_default())
                        .unwrap_or_default();
                    frames.push(FrameInput {
                        frame_id: id.clone(),
                        parent_frame_id: parent,
                        owner_backend_id: owner,
                        session_id: job.session.clone(),
                        loader_id: f["loaderId"].as_str().map(str::to_string),
                        ax_nodes: ax,
                        extras: Vec::new(),
                        extras_failed: false,
                    });
                    for c in node["childFrames"].as_array().into_iter().flatten() {
                        stack.push((c.clone(), false, Some(id.clone()), None));
                    }
                }
            }
            // Out-of-process iframes announced by auto-attach but absent from the parent's tree.
            let pending: Vec<String> = self
                .shared
                .oopif
                .lock()
                .map(|m| {
                    m.values()
                        .filter(|s| !visited.contains(*s))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if pending.is_empty() {
                break;
            }
            for s in pending {
                visited.insert(s.clone());
                queue.push(Job {
                    session: Some(s),
                    parent: None,
                    owner: None,
                });
            }
        }
        // Main frame first.
        frames.sort_by_key(|f| f.parent_frame_id.is_some());
        self.attach_dom_extras(&mut frames).await;
        Ok((frames, main_url))
    }

    async fn frame_owner(&self, session: Option<&str>, frame_id: &str) -> Option<i64> {
        self.cmd_on(session, "DOM.getFrameOwner", json!({"frameId": frame_id}))
            .await
            .ok()?
            .get("backendNodeId")?
            .as_i64()
    }

    /// One `DOMSnapshot.captureSnapshot` per session, distributed to the frames it covers.
    async fn attach_dom_extras(&self, frames: &mut [FrameInput]) {
        let mut sessions: Vec<Option<String>> = Vec::new();
        for f in frames.iter() {
            if !sessions.contains(&f.session_id) {
                sessions.push(f.session_id.clone());
            }
        }
        for s in sessions {
            let mut captured = None;
            for _attempt in 0..2 {
                if let Ok(r) = self
                    .cmd_on(
                        s.as_deref(),
                        "DOMSnapshot.captureSnapshot",
                        json!({"computedStyles": COMPUTED_STYLES}),
                    )
                    .await
                {
                    captured = Some(r);
                    break;
                }
            }
            let Some(resp) = captured else {
                // Fail closed: without DOM facts sensitive inputs are unrecognisable.
                for f in frames.iter_mut().filter(|f| f.session_id == s) {
                    f.extras_failed = true;
                }
                continue;
            };
            let extras: Vec<DomExtra> = dom::parse_dom_snapshot(&resp);
            let root = frames
                .iter()
                .find(|f| f.session_id == s && (s.is_some() || f.parent_frame_id.is_none()))
                .map(|f| f.frame_id.clone());
            for e in extras {
                let target = e.frame_id.clone().or_else(|| root.clone());
                if let Some(f) = frames
                    .iter_mut()
                    .find(|f| f.session_id == s && Some(&f.frame_id) == target.as_ref())
                {
                    f.extras.push(e);
                }
            }
        }
    }

    // -- snapshots ------------------------------------------------------------------------------

    /// Title from `Target.getTargetInfo` (falls back to `document.title`). Chrome reports the URL
    /// as the title of untitled pages; that is mapped back to an empty title.
    async fn target_title(&self, url: &str) -> String {
        let info = self
            .client
            .send("Target.getTargetInfo", json!({"targetId": self.target_id}))
            .await
            .ok()
            .and_then(|v| v.pointer("/targetInfo/title")?.as_str().map(str::to_string));
        match info {
            Some(t) => {
                let bare = url.split_once("://").map_or(url, |(_, r)| r);
                if t == url || t == bare {
                    String::new()
                } else {
                    t
                }
            }
            None => self.title().await.unwrap_or_default(),
        }
    }

    /// Set the options later actions use for their post-action snapshot.
    pub async fn set_snapshot_options(&self, opts: &SnapshotOptions) {
        self.state.lock().await.opts = opts.clone();
    }

    /// Number of AX/DOMSnapshot rounds taken so far (diagnostics).
    #[doc(hidden)]
    pub fn snapshot_rounds(&self) -> u64 {
        self.shared.snap_rounds.load(Ordering::SeqCst)
    }

    /// Capture a snapshot with the given options and remember it as the baseline for diffs.
    pub async fn snapshot(&self, opts: &SnapshotOptions) -> Result<Snapshot> {
        let gen_now = self.shared.nav_gen.load(Ordering::SeqCst);
        let (frames, url) = self.collect_frames().await?;
        let title = self.target_title(&url).await;
        let mut st = self.state.lock().await;
        if st.gen_seen != gen_now {
            st.refs.clear();
        }
        st.gen_seen = gen_now;
        let nodes = build_snapshot(&frames, opts, &mut st.refs);
        let snap = Snapshot { url, title, nodes };
        st.last = Some(snap.clone());
        st.opts = opts.clone();
        st.frames = frames
            .iter()
            .map(|f| FrameMeta {
                id: f.frame_id.clone(),
                parent: f.parent_frame_id.clone(),
                owner_backend: f.owner_backend_id,
                session: f.session_id.clone(),
            })
            .collect();
        Ok(snap)
    }

    /// Snapshot and diff against the previous one.
    pub async fn snapshot_diff(&self, opts: &SnapshotOptions) -> Result<(Snapshot, SnapshotDiff)> {
        let prev = self.state.lock().await.last.clone();
        let next = self.snapshot(opts).await?;
        let d = match prev {
            Some(p) => diff::diff(&p, &next),
            None => diff::diff(&Snapshot::default(), &next),
        };
        Ok((next, d))
    }

    /// The most recent snapshot, if any.
    pub async fn last_snapshot(&self) -> Option<Snapshot> {
        self.state.lock().await.last.clone()
    }

    /// Forget every ref (they become unknown until the next snapshot).
    pub async fn invalidate_refs(&self) {
        self.state.lock().await.refs.clear();
    }

    /// Resolve a ref issued by the latest snapshot of the current document.
    pub(crate) async fn locate(&self, r: &str) -> Result<RefEntry> {
        let mut st = self.state.lock().await;
        if st.gen_seen != self.shared.nav_gen.load(Ordering::SeqCst) {
            st.refs.clear();
        }
        st.refs
            .get(r)
            .cloned()
            .ok_or_else(|| Error::UnknownRef(RefMap::normalize(r).to_string()))
    }

    // -- navigation -----------------------------------------------------------------------------

    /// Wait for the next `Page.loadEventFired` on the main session.
    pub(crate) async fn wait_load(
        &self,
        rx: &mut broadcast::Receiver<CdpEvent>,
        timeout: Duration,
    ) -> Result<()> {
        self.wait_events(rx, &["Page.loadEventFired"], timeout)
            .await
    }

    /// Wait for the next of `methods` on the main session.
    pub(crate) async fn wait_events(
        &self,
        rx: &mut broadcast::Receiver<CdpEvent>,
        methods: &[&str],
        timeout: Duration,
    ) -> Result<()> {
        let fut = async {
            loop {
                match rx.recv().await {
                    Ok(ev)
                        if methods.contains(&ev.method.as_str())
                            && ev.session_id.as_deref() == Some(&self.session) =>
                    {
                        return Ok(());
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::Closed),
                }
            }
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| Error::Timeout("page load".into()))?
    }

    /// Requests in flight that are younger than [`INFLIGHT_CAP`].
    pub(crate) fn busy_requests(&self) -> usize {
        busy_count(&self.shared)
    }

    /// Wait until no network request has been in flight for `quiet`, up to `timeout`.
    pub(crate) async fn wait_network_idle(&self, quiet: Duration, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let inflight = self.busy_requests();
            let last = self.shared.last_net.lock().ok().and_then(|l| *l);
            let idle_for = last.map_or(quiet, |t| t.elapsed());
            if inflight == 0 && idle_for >= quiet {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout("network idle".into()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Background task: tracks OOPIF sessions, network activity, navigations, and target loss for one page.
async fn watch(
    mut rx: broadcast::Receiver<CdpEvent>,
    session: String,
    target_id: String,
    shared: Arc<Shared>,
    client: Option<Arc<CdpClient>>,
) {
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => {
                mark_target_closed(&shared);
                return;
            }
        };
        if ev.session_id.is_none() {
            match ev.method.as_str() {
                "Target.detachedFromTarget"
                    if ev.params["sessionId"].as_str() == Some(session.as_str())
                        || ev.params["targetId"].as_str() == Some(target_id.as_str()) =>
                {
                    mark_target_closed(&shared);
                }
                "Target.targetDestroyed" | "Target.targetCrashed"
                    if ev.params["targetId"].as_str() == Some(target_id.as_str()) =>
                {
                    mark_target_closed(&shared);
                }
                _ => {}
            }
            continue;
        }
        let Some(sid) = ev.session_id.as_deref() else {
            continue;
        };
        let known = sid == session
            || shared
                .oopif
                .lock()
                .is_ok_and(|m| m.values().any(|v| v == sid));
        if !known {
            continue;
        }
        match ev.method.as_str() {
            "Target.attachedToTarget" => {
                let info = &ev.params["targetInfo"];
                if info["type"].as_str() == Some("iframe")
                    && let (Some(tid), Some(csid)) =
                        (info["targetId"].as_str(), ev.params["sessionId"].as_str())
                    && let Ok(mut m) = shared.oopif.lock()
                {
                    m.insert(tid.to_string(), csid.to_string());
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(csid) = ev.params["sessionId"].as_str() {
                    if csid == session {
                        mark_target_closed(&shared);
                    }
                    if let Ok(mut m) = shared.oopif.lock() {
                        m.retain(|_, v| v != csid);
                    }
                    if let Ok(mut r) = shared.ready.lock() {
                        r.remove(csid);
                    }
                }
            }
            "Inspector.targetCrashed" | "Target.targetCrashed" if sid == session => {
                mark_target_closed(&shared);
            }
            "Network.requestWillBeSent" if sid == session => {
                if let Ok(mut map) = shared.inflight.lock() {
                    map.retain(|_, t| t.elapsed() < INFLIGHT_CAP);
                    if let Some(id) = ev.params["requestId"].as_str() {
                        map.entry(id.to_string()).or_insert_with(Instant::now);
                    }
                }
                touch(&shared);
            }
            "Network.loadingFinished" | "Network.loadingFailed" if sid == session => {
                if let Ok(mut map) = shared.inflight.lock() {
                    map.retain(|_, t| t.elapsed() < INFLIGHT_CAP);
                    if let Some(id) = ev.params["requestId"].as_str() {
                        map.remove(id);
                    }
                }
                touch(&shared);
            }
            "Page.javascriptDialogOpening" => {
                let raw_type = ev.params.get("type").and_then(Value::as_str);
                let kind = raw_type.unwrap_or("unknown").to_string();
                let policy = shared
                    .dialog_policy
                    .lock()
                    .map(|p| p.clone())
                    .unwrap_or_default();
                let (accept, prompt_text) = match policy.action {
                    DialogAction::Accept => (true, policy.prompt_text.clone()),
                    DialogAction::Dismiss => (false, None),
                    DialogAction::Auto => (default_accepts_dialog(&kind), None),
                };
                if let Ok(mut d) = shared.dialogs.lock() {
                    d.push(DialogInfo {
                        kind,
                        message: ev.params["message"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        accepted: accept,
                    });
                }
                if let Some(c) = &client {
                    // Spawned: the dialog blocks the page command that caused it, and the
                    // watcher must keep draining events meanwhile.
                    let c = c.clone();
                    let sid = sid.to_string();
                    tokio::spawn(async move {
                        let mut params = json!({"accept": accept});
                        if let Some(txt) = prompt_text {
                            params["promptText"] = json!(txt);
                        }
                        let _ = c
                            .send_on(Some(&sid), "Page.handleJavaScriptDialog", params)
                            .await;
                    });
                }
            }
            "Page.frameNavigated"
                if sid == session && ev.params["frame"].get("parentId").is_none() =>
            {
                shared.nav_gen.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut set) = shared.inflight.lock() {
                    set.clear();
                }
                notify_action_hooks(&shared);
            }
            _ => {}
        }
    }
}

fn mark_target_closed(shared: &Shared) {
    shared.target_closed.store(true, Ordering::SeqCst);
    shared.target_close_notify.notify_waiters();
}

fn notify_action_hooks(shared: &Shared) {
    let callbacks: Vec<Arc<dyn Fn() + Send + Sync>> = shared
        .live_view_action_hooks
        .lock()
        .ok()
        .map(|mut hooks| {
            let live: Vec<_> = hooks.iter().filter_map(std::sync::Weak::upgrade).collect();
            hooks.retain(|w| w.strong_count() > 0);
            live
        })
        .unwrap_or_default();
    for cb in callbacks {
        cb();
    }
}

fn busy_count(shared: &Shared) -> usize {
    shared.inflight.lock().map_or(0, |mut m| {
        m.retain(|_, t| t.elapsed() < INFLIGHT_CAP);
        m.len()
    })
}

fn touch(shared: &Shared) {
    if let Ok(mut l) = shared.last_net.lock() {
        *l = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(method: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.into(),
            params,
            session_id: Some("S".into()),
        }
    }

    #[tokio::test]
    async fn redirect_chain_does_not_leak_inflight() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        // Three hops share one requestId, then a single loadingFinished.
        for _ in 0..3 {
            tx.send(ev("Network.requestWillBeSent", json!({"requestId": "R1"})))
                .unwrap();
        }
        tx.send(ev("Network.loadingFinished", json!({"requestId": "R1"})))
            .unwrap();
        drop(tx);
        task.await.unwrap();
        assert!(shared.inflight.lock().unwrap().is_empty());
        assert_eq!(busy_count(&shared), 0);
    }

    #[tokio::test]
    async fn old_long_poll_does_not_block_idle() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        tx.send(ev(
            "Network.requestWillBeSent",
            json!({"requestId": "poll"}),
        ))
        .unwrap();
        drop(tx);
        task.await.unwrap();
        assert_eq!(busy_count(&shared), 1, "a fresh request is busy");
        let old = Instant::now()
            .checked_sub(INFLIGHT_CAP + Duration::from_secs(1))
            .unwrap();
        shared.inflight.lock().unwrap().insert("poll".into(), old);
        assert_eq!(busy_count(&shared), 0, "an old long-poll is ignored");
        assert!(
            shared.inflight.lock().unwrap().is_empty(),
            "expired in-flight request must be pruned from map"
        );
    }

    #[tokio::test]
    async fn expired_inflight_is_pruned_on_new_request() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        let old = Instant::now()
            .checked_sub(INFLIGHT_CAP + Duration::from_secs(1))
            .unwrap();
        shared
            .inflight
            .lock()
            .unwrap()
            .insert("old_poll".into(), old);
        assert_eq!(shared.inflight.lock().unwrap().len(), 1);
        tx.send(ev(
            "Network.requestWillBeSent",
            json!({"requestId": "new_req"}),
        ))
        .unwrap();
        drop(tx);
        task.await.unwrap();
        let map = shared.inflight.lock().unwrap();
        assert!(
            !map.contains_key("old_poll"),
            "expired request was pruned from the map"
        );
        assert!(map.contains_key("new_req"));
    }

    #[tokio::test]
    async fn redirect_hops_keep_first_seen_time() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        tx.send(ev("Network.requestWillBeSent", json!({"requestId": "R"})))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let first = *shared.inflight.lock().unwrap().get("R").unwrap();
        tx.send(ev("Network.requestWillBeSent", json!({"requestId": "R"})))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(tx);
        task.await.unwrap();
        assert_eq!(*shared.inflight.lock().unwrap().get("R").unwrap(), first);
    }

    #[tokio::test]
    async fn dialog_events_are_recorded_with_default_policy() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        for (t, m) in [
            ("alert", "a"),
            ("confirm", "c"),
            ("prompt", "p"),
            ("beforeunload", "b"),
        ] {
            tx.send(ev(
                "Page.javascriptDialogOpening",
                json!({"type": t, "message": m}),
            ))
            .unwrap();
        }
        // Missing type -> treated as unknown and dismissed
        tx.send(ev(
            "Page.javascriptDialogOpening",
            json!({"message": "mystery"}),
        ))
        .unwrap();
        drop(tx);
        task.await.unwrap();
        let d = shared.dialogs.lock().unwrap().clone();
        let got: Vec<(&str, bool)> = d.iter().map(|x| (x.kind.as_str(), x.accepted)).collect();
        assert_eq!(
            got,
            [
                ("alert", true),
                ("confirm", false),
                ("prompt", false),
                ("beforeunload", false),
                ("unknown", false),
            ]
        );
        assert_eq!(d[1].message, "c");
        assert_eq!(d[4].kind, "unknown");
    }

    #[tokio::test]
    async fn dialog_events_deliberate_accept() {
        let (tx, rx) = broadcast::channel(64);
        let shared = Arc::new(Shared::default());
        *shared.dialog_policy.lock().unwrap() = DialogOptions::accept_prompt("hello");
        let task = tokio::spawn(watch(rx, "S".into(), "T".into(), shared.clone(), None));
        for (t, m) in [("confirm", "c"), ("prompt", "p"), ("beforeunload", "b")] {
            tx.send(ev(
                "Page.javascriptDialogOpening",
                json!({"type": t, "message": m}),
            ))
            .unwrap();
        }
        drop(tx);
        task.await.unwrap();
        let d = shared.dialogs.lock().unwrap().clone();
        let got: Vec<(&str, bool)> = d.iter().map(|x| (x.kind.as_str(), x.accepted)).collect();
        assert_eq!(
            got,
            [("confirm", true), ("prompt", true), ("beforeunload", true),]
        );
    }
}
