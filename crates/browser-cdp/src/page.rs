// Adapted from vercel-labs/agent-browser (Apache-2.0): frame discovery, AX + DOM capture, load waiting.
//! A single tab: session management, frames (including out-of-process iframes), snapshots.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;

use crate::client::{CdpClient, CdpEvent};
use crate::diff::{self, SnapshotDiff};
use crate::dom::{self, COMPUTED_STYLES};
use crate::error::{Error, Result};
use crate::snapshot::{
    DomExtra, FrameInput, RefEntry, RefMap, Snapshot, SnapshotOptions, build_snapshot,
};

/// Default navigation / wait timeout.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// Shared with the background event watcher.
#[derive(Default)]
pub(crate) struct Shared {
    /// Frame id -> flattened session of out-of-process iframes.
    pub oopif: StdMutex<HashMap<String, String>>,
    /// Sessions whose domains are already enabled.
    pub ready: StdMutex<HashSet<String>>,
    /// In-flight network request ids of the main session (a redirect chain reuses one id).
    pub inflight: StdMutex<HashSet<String>>,
    /// Last network activity.
    pub last_net: StdMutex<Option<Instant>>,
    /// Incremented on every main-frame navigation; invalidates refs.
    pub nav_gen: AtomicU64,
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
        let watcher = tokio::spawn(watch(rx, session.clone(), shared.clone()));
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

    /// Target id of this tab.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// Flattened session id of this tab.
    pub fn session_id(&self) -> &str {
        &self.session
    }

    /// Detach the session (the tab keeps running).
    pub async fn detach(&self) -> Result<()> {
        self.client
            .send(
                "Target.detachFromTarget",
                json!({"sessionId": self.session}),
            )
            .await
            .map(|_| ())
    }

    /// Close the tab.
    pub async fn close(&self) -> Result<()> {
        self.client
            .send("Target.closeTarget", json!({"targetId": self.target_id}))
            .await
            .map(|_| ())
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
    async fn collect_frames(&self) -> Result<Vec<FrameInput>> {
        struct Job {
            session: Option<String>,
            parent: Option<String>,
            owner: Option<i64>,
        }
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
        Ok(frames)
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

    /// Capture a snapshot with the given options and remember it as the baseline for diffs.
    pub async fn snapshot(&self, opts: &SnapshotOptions) -> Result<Snapshot> {
        let gen_now = self.shared.nav_gen.load(Ordering::SeqCst);
        let frames = self.collect_frames().await?;
        let url = self.url().await.unwrap_or_default();
        let title = self.title().await.unwrap_or_default();
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
        let fut = async {
            loop {
                match rx.recv().await {
                    Ok(ev)
                        if ev.method == "Page.loadEventFired"
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

    /// Wait until no network request has been in flight for `quiet`, up to `timeout`.
    pub(crate) async fn wait_network_idle(&self, quiet: Duration, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let inflight = self.shared.inflight.lock().map_or(0, |s| s.len());
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

/// Background task: tracks OOPIF sessions, network activity and navigations for one page.
async fn watch(mut rx: broadcast::Receiver<CdpEvent>, session: String, shared: Arc<Shared>) {
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
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
                    if let Ok(mut m) = shared.oopif.lock() {
                        m.retain(|_, v| v != csid);
                    }
                    if let Ok(mut r) = shared.ready.lock() {
                        r.remove(csid);
                    }
                }
            }
            "Network.requestWillBeSent" if sid == session => {
                if let Some(id) = ev.params["requestId"].as_str()
                    && let Ok(mut set) = shared.inflight.lock()
                {
                    set.insert(id.to_string());
                }
                touch(&shared);
            }
            "Network.loadingFinished" | "Network.loadingFailed" if sid == session => {
                if let Some(id) = ev.params["requestId"].as_str()
                    && let Ok(mut set) = shared.inflight.lock()
                {
                    set.remove(id);
                }
                touch(&shared);
            }
            "Page.frameNavigated"
                if sid == session && ev.params["frame"].get("parentId").is_none() =>
            {
                shared.nav_gen.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut set) = shared.inflight.lock() {
                    set.clear();
                }
            }
            _ => {}
        }
    }
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
        let task = tokio::spawn(watch(rx, "S".into(), shared.clone()));
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
    }
}
