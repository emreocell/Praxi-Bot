// Click-at-box-center approach follows browser-harness (MIT); structure adapted from
// vercel-labs/agent-browser (Apache-2.0).
//! Actions by ref. Every action returns an [`ActionResult`] carrying the refreshed view as a diff.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::diff::{self, SnapshotDiff};
use crate::error::{Error, Result};
use crate::page::{DEFAULT_WAIT, DialogInfo, DialogOptions, NAVIGATION_LOAD_DEADLINE, Page};
use crate::snapshot::{DomExtra, RefEntry, Snapshot};

/// Result of an action: success flag, refreshed view as a diff against what the agent saw before.
#[derive(Debug, Clone)]
pub struct ActionResult {
    /// False when the action could not be performed (stale ref, element not visible, ...).
    pub ok: bool,
    /// Why it failed when `ok` is false.
    pub error: Option<String>,
    /// Changes between the previous snapshot and the post-action snapshot.
    pub new_snapshot_diff: SnapshotDiff,
    /// The full post-action snapshot (new baseline).
    pub snapshot: Snapshot,
    /// All JS dialogs that opened during the action and were answered.
    pub dialogs: Vec<DialogInfo>,
    /// Set when the page did not settle (for example `network still busy: 2 requests`).
    pub settle_note: Option<String>,
}

impl ActionResult {
    /// First JS dialog that opened during the action, if any.
    pub fn dialog(&self) -> Option<&DialogInfo> {
        self.dialogs.first()
    }
}

/// Condition for [`Page::wait_for`].
#[derive(Debug, Clone)]
pub enum WaitCondition {
    /// A CSS selector matches an element in the main frame.
    Selector(String),
    /// The main frame's visible text contains the string.
    Text(String),
    /// No network activity for 500 ms.
    NetworkIdle,
}

/// What a screenshot covers.
#[derive(Debug, Clone)]
pub enum ScreenshotTarget {
    /// Current viewport.
    Viewport,
    /// Whole scrollable page.
    FullPage,
    /// One element by ref.
    Element(String),
}

/// Screenshot options.
#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    /// Area to capture.
    pub target: ScreenshotTarget,
    /// JPEG quality 1-100; `None` produces a PNG.
    pub jpeg_quality: Option<u8>,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            target: ScreenshotTarget::Viewport,
            jpeg_quality: Some(80),
        }
    }
}

const MOD_ALT: u32 = 1;
const MOD_CTRL: u32 = 2;
const MOD_META: u32 = 4;
const MOD_SHIFT: u32 = 8;

/// A resolved key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// `KeyboardEvent.code`.
    pub code: String,
    /// Windows virtual key code.
    pub vk: u32,
    /// Text inserted by the key, if any.
    pub text: Option<String>,
    /// Modifier bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8).
    pub modifiers: u32,
}

/// Error text of [`Page::focused_entry`] when no element holds focus.
const NO_FOCUS: &str = "no element has keyboard focus";

/// `Enter` carries text for the page but never type into a field.
fn is_navigation_spec(spec: &str) -> bool {
    matches!(
        spec.rsplit('+')
            .next()
            .unwrap_or(spec)
            .to_ascii_lowercase()
            .as_str(),
        "enter" | "return"
    )
}

/// Keys that activate the focused control (Enter, Space), alone or in a chord.
fn is_activating_spec(spec: &str) -> bool {
    matches!(
        spec.rsplit('+')
            .next()
            .unwrap_or(spec)
            .to_ascii_lowercase()
            .as_str(),
        "enter" | "return" | "numpadenter" | " " | "space" | "spacebar"
    )
}

/// What the browser reports as the focused element of the top document.
enum ActiveElement {
    Nothing,
    Node {
        backend_id: i64,
        hosts_other_document: bool,
    },
}

/// Extra checks one step of a `browser_act` batch runs under the session lock, on top of
/// the checks of the single tool. `Default` (no batch) changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepGuard {
    /// The step is part of a batch: a click must land on its ref's node, and an activating
    /// key needs `focus_ref`.
    pub batch: bool,
    /// The ref an earlier step of the batch worked on; focus must still be on that node.
    pub focus_ref: Option<String>,
}

/// Parse `Enter`, `Tab`, `ArrowDown`, `a`, `Control+a`, `Shift+Tab`, ...
pub fn parse_key(spec: &str) -> Result<KeySpec> {
    let parts: Vec<&str> = if spec == "+" {
        vec!["+"]
    } else {
        spec.split('+').collect()
    };
    let (main, mods) = parts
        .split_last()
        .ok_or_else(|| Error::Element("empty key".into()))?;
    let mut modifiers = 0;
    for m in mods {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" | "option" => MOD_ALT,
            "ctrl" | "control" => MOD_CTRL,
            "meta" | "cmd" | "command" | "win" => MOD_META,
            "shift" => MOD_SHIFT,
            other => return Err(Error::Element(format!("unknown modifier '{other}'"))),
        };
    }
    let named: Option<(&str, &str, u32, Option<&str>)> = match main.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(("Enter", "Enter", 13, Some("\r"))),
        "tab" => Some(("Tab", "Tab", 9, None)),
        "escape" | "esc" => Some(("Escape", "Escape", 27, None)),
        "backspace" => Some(("Backspace", "Backspace", 8, None)),
        "delete" | "del" => Some(("Delete", "Delete", 46, None)),
        "space" => Some((" ", "Space", 32, Some(" "))),
        "arrowup" | "up" => Some(("ArrowUp", "ArrowUp", 38, None)),
        "arrowdown" | "down" => Some(("ArrowDown", "ArrowDown", 40, None)),
        "arrowleft" | "left" => Some(("ArrowLeft", "ArrowLeft", 37, None)),
        "arrowright" | "right" => Some(("ArrowRight", "ArrowRight", 39, None)),
        "home" => Some(("Home", "Home", 36, None)),
        "end" => Some(("End", "End", 35, None)),
        "pageup" => Some(("PageUp", "PageUp", 33, None)),
        "pagedown" => Some(("PageDown", "PageDown", 34, None)),
        _ => None,
    };
    if let Some((key, code, vk, text)) = named {
        return Ok(KeySpec {
            key: key.into(),
            code: code.into(),
            vk,
            text: text.map(str::to_string),
            modifiers,
        });
    }
    let mut chars = main.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(Error::Element(format!("unknown key '{main}'")));
    };
    let up = c.to_ascii_uppercase();
    let code = if c.is_ascii_alphabetic() {
        format!("Key{up}")
    } else if c.is_ascii_digit() {
        format!("Digit{c}")
    } else {
        String::new()
    };
    let shifted = modifiers & MOD_SHIFT != 0 && c.is_ascii_alphabetic();
    let key = if shifted {
        up.to_string()
    } else {
        c.to_string()
    };
    let text = (modifiers & (MOD_CTRL | MOD_ALT | MOD_META) == 0).then(|| key.clone());
    Ok(KeySpec {
        key,
        code,
        vk: if up.is_ascii() { u32::from(up) } else { 0 },
        text,
        modifiers,
    })
}

fn quad_center(q: &[f64]) -> Option<(f64, f64)> {
    if q.len() < 8 {
        return None;
    }
    let xs = q.iter().step_by(2);
    let ys = q.iter().skip(1).step_by(2);
    Some((xs.sum::<f64>() / 4.0, ys.sum::<f64>() / 4.0))
}

/// Whether the `DOM.describeNode` tree `n` holds the backend node `id` (shadow roots, frame
/// documents and template content included).
fn node_contains(n: &Value, id: i64) -> bool {
    if n["backendNodeId"].as_i64() == Some(id) {
        return true;
    }
    let kids = |k: &str| n[k].as_array().map_or(&[][..], Vec::as_slice);
    kids("children")
        .iter()
        .chain(kids("shadowRoots"))
        .chain(n.get("contentDocument"))
        .chain(n.get("templateContent"))
        .any(|c| node_contains(c, id))
}

fn floats(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

/// The document a frame currently shows, as the browser reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameIdentity {
    /// `scheme://host[:port]`, `None` for an opaque origin.
    pub origin: Option<String>,
    /// The loader (document) id: changes when the frame navigates.
    pub loader_id: Option<String>,
}

/// What an approval card and the allow-list check need to know about a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldTarget {
    /// The field's accessible name (page-supplied text: label it as such).
    pub name: String,
    /// The field's role.
    pub role: String,
    /// Origin of the FRAME that holds the field (`None` = opaque / unknown).
    pub frame_origin: Option<String>,
    /// Origin of the top-level document (`None` = opaque / unknown).
    pub top_origin: Option<String>,
    /// The field holds a secret (password, one-time code, card number).
    pub sensitive: bool,
}

/// An `http(s)` origin as Chrome reports it, or `None` for `null`, `://`,
/// empty or any other scheme (an opaque origin matches nothing).
fn web_origin(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;
    (!rest.is_empty() && !rest.contains(['/', '?', '#', ' ', '@'])).then(|| raw.to_owned())
}

fn grant_origin_matches(origin: Option<&str>, expected: &str) -> bool {
    let canonical = |raw: &str| -> Option<String> {
        let u = url::Url::parse(raw).ok()?;
        if !matches!(u.scheme(), "http" | "https") {
            return None;
        }
        Some(format!(
            "{}://{}:{}",
            u.scheme(),
            u.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
            u.port_or_known_default()?
        ))
    };
    origin
        .and_then(canonical)
        .is_some_and(|o| Some(o) == canonical(expected))
}

impl Page {
    /// Resolve the actual keyboard receiver through browser DOM focus state.
    /// Unknown/unavailable child sessions fail closed rather than borrowing the top origin.
    async fn focused_entry(&self) -> Result<RefEntry> {
        let tree = self.cmd("Page.getFrameTree", json!({})).await?;
        let mut frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| Error::Element("unknown focused frame".into()))?
            .to_owned();
        let mut session: Option<String> = None;
        let doc = self.cmd("DOM.getDocument", json!({})).await?;
        let mut root = doc["root"]["nodeId"].clone();
        for _ in 0..32 {
            let focus = self
                .cmd_on(
                    session.as_deref(),
                    "DOM.querySelector",
                    json!({"nodeId": root, "selector": ":focus"}),
                )
                .await?;
            if focus["nodeId"].as_i64().unwrap_or(0) == 0 {
                return Err(Error::Element(NO_FOCUS.into()));
            }
            let node = self
                .cmd_on(
                    session.as_deref(),
                    "DOM.describeNode",
                    json!({"nodeId": focus["nodeId"], "depth": 1, "pierce": true}),
                )
                .await?;
            let node = &node["node"];
            if let Some(shadow) = node["shadowRoots"]
                .as_array()
                .and_then(|roots| roots.first())
            {
                root = shadow["nodeId"].clone();
                continue;
            }
            if matches!(node["nodeName"].as_str(), Some("IFRAME" | "FRAME")) {
                frame = node["frameId"]
                    .as_str()
                    .ok_or_else(|| Error::Element("unknown focused child frame".into()))?
                    .to_owned();
                if let Some(child) = node.get("contentDocument") {
                    root = child["nodeId"].clone();
                } else {
                    session = self
                        .shared
                        .oopif
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&frame).cloned());
                    if session.is_none() {
                        return Err(Error::Element("unknown focused child session".into()));
                    }
                    let doc = self
                        .cmd_on(session.as_deref(), "DOM.getDocument", json!({}))
                        .await?;
                    root = doc["root"]["nodeId"].clone();
                }
            } else {
                return Ok(RefEntry {
                    session_id: session,
                    frame_id: frame,
                    backend_id: node["backendNodeId"]
                        .as_i64()
                        .ok_or_else(|| Error::Element("unknown focused node".into()))?,
                    name: String::new(),
                    role: String::new(),
                    // Unknown until described; `focused_target` fills it in, failing closed.
                    sensitive: true,
                });
            }
        }
        Err(Error::Element("focused frame nesting limit".into()))
    }

    /// Browser-reported origin of the receiving frame; never infer it from a URL.
    pub async fn input_origin(&self, r: Option<&str>) -> Result<Option<String>> {
        let e = match r {
            Some(r) => self.locate(r).await?,
            None => self.focused_entry().await?,
        };
        Ok(self.frame_identity(&e).await?.origin)
    }

    async fn check_input_origin(&self, e: &RefEntry, expected: Option<&str>) -> Result<()> {
        if let Some(expected) = expected
            && !grant_origin_matches(self.frame_identity(e).await?.origin.as_deref(), expected)
        {
            return Err(Error::Element(
                "receiving frame origin changed; ask again before input".into(),
            ));
        }
        Ok(())
    }

    // -- result plumbing ------------------------------------------------------------------------

    /// Baseline for diffing: the last snapshot the agent saw (taking one if there is none yet).
    async fn baseline_with(&self, opts: &DialogOptions) -> Result<Snapshot> {
        if let Ok(mut d) = self.shared.dialogs.lock() {
            d.clear();
        }
        if let Ok(mut p) = self.shared.dialog_policy.lock() {
            *p = opts.clone();
        }
        if let Ok(mut m) = self.shared.action_mark.lock() {
            *m = Some((Instant::now(), self.shared.nav_gen.load(Ordering::SeqCst)));
        }
        let last = self.state.lock().await.last.clone();
        if let Some(s) = last {
            return Ok(s);
        }
        let snap_opts = self.state.lock().await.opts.clone();
        self.snapshot(&snap_opts).await
    }

    async fn baseline(&self) -> Result<Snapshot> {
        self.baseline_with(&DialogOptions::default()).await
    }

    /// Wait for the page to settle after an action, take a new snapshot, diff, and package.
    async fn finish(&self, before: Snapshot, outcome: Result<()>) -> Result<ActionResult> {
        self.finish_with(before, outcome, true).await
    }

    /// Like [`Page::finish`]; `settle = false` skips the settle wait (the caller already knows
    /// the page is in the wanted state).
    async fn finish_with(
        &self,
        before: Snapshot,
        outcome: Result<()>,
        settle: bool,
    ) -> Result<ActionResult> {
        if let Err(e) = &outcome
            && e.is_fatal()
        {
            return Err(outcome.unwrap_err());
        }
        let t_settle = Instant::now();
        let settle_note = if settle { self.settle().await } else { None };
        let settle_ms = t_settle.elapsed().as_millis();
        self.shared.dom_gen.fetch_add(1, Ordering::SeqCst);
        let t_snap = Instant::now();
        let opts = self.state.lock().await.opts.clone();
        let snapshot = self.snapshot(&opts).await?;
        // Per-step timing, no page content: where the time of one action went.
        tracing::debug!(
            target: "browser_cdp::timing",
            settle_ms = u64::try_from(settle_ms).unwrap_or(u64::MAX),
            snapshot_ms = u64::try_from(t_snap.elapsed().as_millis()).unwrap_or(u64::MAX),
            nodes = snapshot.nodes.len(),
            settled = settle,
            "browser action finished"
        );
        if let Ok(mut m) = self.shared.action_mark.lock() {
            *m = None;
        }
        let new_snapshot_diff = diff::diff(&before, &snapshot);
        let dialogs = self
            .shared
            .dialogs
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default();
        if let Ok(mut p) = self.shared.dialog_policy.lock() {
            *p = DialogOptions::default();
        }
        if outcome.is_ok() {
            self.notify_live_view_action();
        }
        Ok(ActionResult {
            ok: outcome.is_ok(),
            error: outcome.err().map(|e| e.to_string()),
            new_snapshot_diff,
            snapshot,
            dialogs,
            settle_note,
        })
    }

    /// Give navigation, rendering and XHRs a chance to finish (best effort). Returns a note when
    /// the network was still busy at the end.
    async fn settle(&self) -> Option<String> {
        let mark = self.shared.action_mark.lock().ok().and_then(|m| *m);
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Nothing navigated and nothing new went on the wire since the action began: there is
        // nothing to wait for. (Without this, one long poll or beacon that was already open
        // cost every single click the full network-idle timeout.)
        if let Some((since, gen_then)) = mark
            && !self.activity_since(since, gen_then)
        {
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut load_complete = false;
        while Instant::now() < deadline {
            let ready = tokio::time::timeout(
                Duration::from_secs(2),
                self.evaluate_readonly("document.readyState"),
            )
            .await;
            match ready {
                Ok(Ok(v)) if v.as_str() == Some("complete") => {
                    load_complete = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let navigated = mark.is_some_and(|(_, g)| self.shared.nav_gen.load(Ordering::SeqCst) != g);
        let nav_at = if navigated {
            self.shared.last_nav_at.lock().ok().and_then(|t| *t)
        } else {
            None
        };
        let since = match (mark.map(|(s, _)| s), nav_at) {
            (Some(s), Some(n)) => Some(s.max(n)),
            (Some(s), None) => Some(s),
            (None, n) => n,
        };
        self.wait_network_idle_inner(
            Duration::from_millis(250),
            Duration::from_secs(2),
            since,
            navigated && load_complete,
        )
        .await
        .err()
        .map(|_| format!("network still busy: {} requests", self.busy_requests()))
    }

    // -- geometry -------------------------------------------------------------------------------

    /// Content-box quad of a node, in the coordinates of its own frame's viewport.
    async fn quad(&self, session: Option<&str>, backend_id: i64, kind: &str) -> Result<Vec<f64>> {
        let r = self
            .cmd_on(
                session,
                "DOM.getBoxModel",
                json!({"backendNodeId": backend_id}),
            )
            .await
            .map_err(|e| Error::Element(format!("no box model (detached or hidden?): {e}")))?;
        let q = floats(&r["model"][kind]);
        if q.len() < 8 {
            return Err(Error::Element("element has no layout box".into()));
        }
        Ok(q)
    }

    /// Offset of an out-of-process frame's viewport inside the top-level viewport.
    async fn frame_offset(&self, frame_id: &str) -> (f64, f64) {
        let metas = self.state.lock().await.frames.clone();
        let (mut ox, mut oy) = (0.0, 0.0);
        let mut cur = metas.iter().find(|m| m.id == frame_id).cloned();
        let mut guard = 0;
        while let Some(m) = cur {
            guard += 1;
            if m.session.is_none() || guard > 8 {
                break;
            }
            let Some(parent) = m
                .parent
                .as_ref()
                .and_then(|p| metas.iter().find(|x| &x.id == p))
            else {
                break;
            };
            if let Some(owner) = m.owner_backend
                && let Ok(q) = self.quad(parent.session.as_deref(), owner, "content").await
            {
                ox += q[0];
                oy += q[1];
            }
            cur = Some(parent.clone());
        }
        (ox, oy)
    }

    /// Scroll into view and compute the click point (top-level viewport coordinates).
    async fn center(&self, e: &RefEntry) -> Result<(f64, f64)> {
        let _ = self
            .cmd_on(
                e.session_id.as_deref(),
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": e.backend_id}),
            )
            .await;
        let q = self
            .quad(e.session_id.as_deref(), e.backend_id, "content")
            .await?;
        let (cx, cy) = quad_center(&q).ok_or_else(|| Error::Element("degenerate box".into()))?;
        let width = (q[2] - q[0]).abs();
        let height = (q[5] - q[1]).abs();
        if width < 1.0 || height < 1.0 {
            return Err(Error::Element(format!(
                "element '{}' has zero size (not visible)",
                e.name
            )));
        }
        let (ox, oy) = self.frame_offset(&e.frame_id).await;
        Ok((cx + ox, cy + oy))
    }

    async fn mouse(&self, kind: &str, x: f64, y: f64, extra: Value) -> Result<()> {
        let mut p = json!({"type": kind, "x": x, "y": y});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        self.cmd("Input.dispatchMouseEvent", p).await.map(|_| ())
    }

    async fn call_on(&self, e: &RefEntry, function: &str, args: &[Value]) -> Result<Value> {
        let obj = self
            .cmd_on(
                e.session_id.as_deref(),
                "DOM.resolveNode",
                json!({"backendNodeId": e.backend_id}),
            )
            .await
            .map_err(|err| Error::Element(format!("element is gone: {err}")))?;
        let object_id = obj["object"]["objectId"]
            .as_str()
            .ok_or_else(|| Error::Element("could not resolve element".into()))?
            .to_string();
        let arguments: Vec<Value> = args.iter().map(|a| json!({"value": a})).collect();
        let r = self
            .cmd_on(
                e.session_id.as_deref(),
                "Runtime.callFunctionOn",
                json!({"objectId": object_id, "functionDeclaration": function,
                       "arguments": arguments, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            return Err(Error::Js(
                ex.pointer("/exception/description")
                    .and_then(Value::as_str)
                    .unwrap_or("exception")
                    .to_string(),
            ));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// The origin and document identity of the frame that holds `e`, as the
    /// BROWSER reports it (`Page.getFrameTree`), never as the page's own
    /// script claims. `origin` is `None` for an opaque origin (sandboxed
    /// frame, `data:`, `about:blank`), which no allow-list can match.
    async fn frame_identity(&self, e: &RefEntry) -> Result<FrameIdentity> {
        let tree = self
            .cmd_on(e.session_id.as_deref(), "Page.getFrameTree", json!({}))
            .await
            .map_err(|err| Error::Element(format!("cannot read the frame of the field: {err}")))?;
        let mut stack = vec![&tree["frameTree"]];
        while let Some(node) = stack.pop() {
            let f = &node["frame"];
            if f["id"].as_str() == Some(e.frame_id.as_str()) {
                return Ok(FrameIdentity {
                    origin: web_origin(f["securityOrigin"].as_str()),
                    loader_id: f["loaderId"].as_str().map(str::to_owned),
                });
            }
            for c in node["childFrames"].as_array().into_iter().flatten() {
                stack.push(c);
            }
        }
        Err(Error::Element(
            "the frame that holds the field is gone".into(),
        ))
    }

    /// Origin of the top-level document, as the browser reports it.
    async fn top_origin(&self) -> Option<String> {
        let tree = self.cmd("Page.getFrameTree", json!({})).await.ok()?;
        web_origin(tree["frameTree"]["frame"]["securityOrigin"].as_str())
    }

    /// Where a field lives right now: its label, role and the origin of the
    /// frame that holds it next to the top-level origin. Used for the approval
    /// card and for the allow-list check, before any value is touched.
    pub async fn field_target(&self, r: &str) -> Result<FieldTarget> {
        let e = self.locate(r).await?;
        let frame = self.frame_identity(&e).await?;
        Ok(FieldTarget {
            name: e.name.clone(),
            role: e.role.clone(),
            frame_origin: frame.origin,
            top_origin: self.top_origin().await,
            // Live check (as `focused_target`): the snapshot flag can be stale, and fails closed on error.
            sensitive: self.entry_is_sensitive(&e).await.unwrap_or(true),
        })
    }

    /// The element that has keyboard focus right now. `sensitive` is true when
    /// it holds a secret OR when that cannot be determined (fail closed).
    pub async fn focused_target(&self) -> Result<FieldTarget> {
        let mut e = self.focused_entry().await?;
        e.sensitive = self.entry_is_sensitive(&e).await.unwrap_or(true);
        let frame = self.frame_identity(&e).await?;
        Ok(FieldTarget {
            name: e.name.clone(),
            role: e.role.clone(),
            frame_origin: frame.origin,
            top_origin: self.top_origin().await,
            sensitive: e.sensitive,
        })
    }

    /// Execution-time sensitivity check: refuses when the element was flagged at snapshot time
    /// or is (or cannot be shown not to be) a secret field right now.
    async fn refuse_if_sensitive_now(&self, e: &RefEntry, what: &str) -> Result<()> {
        let live = self.entry_is_sensitive(e).await.unwrap_or(true);
        if live {
            return Err(Error::Element(format!(
                "refused: the target of this {what} is, or may be, a password / one-time-code / card field; nothing was typed"
            )));
        }
        Ok(())
    }

    /// The top document's `activeElement` as the BROWSER reports it, read in an isolated
    /// world (the page's own scripts cannot patch it there). `Err(())`: cannot tell.
    async fn active_element(&self) -> std::result::Result<ActiveElement, ()> {
        let tree = self
            .cmd("Page.getFrameTree", json!({}))
            .await
            .map_err(|_| ())?;
        let frame_id = tree["frameTree"]["frame"]["id"].as_str().ok_or(())?;
        let world = self
            .cmd(
                "Page.createIsolatedWorld",
                json!({"frameId": frame_id, "worldName": "praxi-focus-check"}),
            )
            .await
            .map_err(|_| ())?;
        let context_id = world["executionContextId"].as_i64().ok_or(())?;
        let r = self
            .cmd(
                "Runtime.evaluate",
                json!({"expression": "document.activeElement", "returnByValue": false,
                       "contextId": context_id}),
            )
            .await
            .map_err(|_| ())?;
        if r.get("exceptionDetails").is_some() {
            return Err(());
        }
        let Some(object_id) = r["result"]["objectId"].as_str() else {
            return Ok(ActiveElement::Nothing);
        };
        let d = self
            .cmd(
                "DOM.describeNode",
                json!({"objectId": object_id, "pierce": true}),
            )
            .await
            .map_err(|_| ())?;
        let node = &d["node"];
        let name = node["nodeName"].as_str().unwrap_or("").to_ascii_uppercase();
        if matches!(name.as_str(), "BODY" | "HTML") {
            return Ok(ActiveElement::Nothing);
        }
        // The user-agent shadow tree of an <input> is not a page-authored host.
        let hosts_other_document = matches!(name.as_str(), "IFRAME" | "FRAME")
            || node["shadowRoots"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|r| r["shadowRootType"].as_str() != Some("user-agent"))
            });
        let backend_id = node["backendNodeId"].as_i64().ok_or(())?;
        Ok(ActiveElement::Node {
            backend_id,
            hosts_other_document,
        })
    }

    /// Fallback for [`Self::refuse_if_sensitive_now`] on the top document's `activeElement`.
    /// A focused frame or shadow host cannot be looked into here, so it counts as unknown.
    async fn refuse_if_active_element_secret(&self) -> Result<()> {
        let refuse = || {
            Err(Error::Element(
                "refused: the target of this key press is, or may be, a password / one-time-code / card field; nothing was typed"
                    .into(),
            ))
        };
        match self.active_element().await {
            Ok(ActiveElement::Nothing) => Ok(()),
            Err(())
            | Ok(ActiveElement::Node {
                hosts_other_document: true,
                ..
            }) => refuse(),
            Ok(ActiveElement::Node { backend_id, .. }) => {
                let entry = RefEntry {
                    session_id: None,
                    frame_id: String::new(),
                    backend_id,
                    name: String::new(),
                    role: String::new(),
                    sensitive: true,
                };
                self.refuse_if_sensitive_now(&entry, "key press").await
            }
        }
    }

    async fn entry_is_sensitive(&self, e: &RefEntry) -> Result<bool> {
        let node = self
            .cmd_on(
                e.session_id.as_deref(),
                "DOM.describeNode",
                json!({"backendNodeId": e.backend_id}),
            )
            .await?;
        let node = &node["node"];
        let tag = node["nodeName"].as_str().unwrap_or("").to_ascii_lowercase();
        let flat: Vec<&str> = node["attributes"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("")).collect())
            .unwrap_or_default();
        let attr_str = |k: &str| {
            flat.chunks(2)
                .find(|p| p.len() == 2 && p[0].eq_ignore_ascii_case(k))
                .map(|p| p[1])
        };
        let attr = |k: &str| attr_str(k).map(str::to_string);
        let is_input = matches!(tag.as_str(), "input" | "textarea");
        let editable = !is_input && crate::dom::is_editable_attrs(attr_str);
        let extra = DomExtra {
            tag,
            input_type: is_input.then(|| attr("type").unwrap_or_else(|| "text".into())),
            autocomplete: attr("autocomplete"),
            name_hints: crate::dom::NAME_HINT_ATTRS
                .iter()
                .filter_map(|k| attr(k))
                .collect(),
            editable,
            ..Default::default()
        };
        Ok(extra.is_sensitive())
    }

    // -- inner actions --------------------------------------------------------------------------

    /// Whether the node that receives a mouse event at the top-level point `(x, y)` is the
    /// node of `e` or a descendant of it (shadow roots included). Anything else on top
    /// (a modal, a cookie banner, a decoy) means the click would not land on what was
    /// approved. A node the browser cannot resolve in the same session counts as no match.
    async fn click_lands_on(&self, e: &RefEntry, x: f64, y: f64) -> bool {
        let Ok(hit) = self
            .cmd(
                "DOM.getNodeForLocation",
                json!({"x": x.round() as i64, "y": y.round() as i64,
                       "includeUserAgentShadowDOM": false, "ignorePointerEventsNone": false}),
            )
            .await
        else {
            return false;
        };
        let Some(hit_id) = hit["backendNodeId"].as_i64() else {
            return false;
        };
        if e.session_id.is_none() && hit_id == e.backend_id {
            return true;
        }
        if e.session_id.is_some() {
            // The top-level hit test only reports the frame element of an out-of-process frame.
            return false;
        }
        // Browser-side only: the ref's subtree (shadow roots and frame documents pierced) as
        // the browser reports it. Page JavaScript never takes part, so a hostile page cannot
        // make `parentNode`/`host` lie.
        let Ok(d) = self
            .cmd(
                "DOM.describeNode",
                json!({"backendNodeId": e.backend_id, "depth": -1, "pierce": true}),
            )
            .await
        else {
            return false;
        };
        node_contains(&d["node"], hit_id)
    }

    async fn click_inner(
        &self,
        r: &str,
        clicks: u32,
        expected: Option<&str>,
        guard: &StepGuard,
    ) -> Result<()> {
        let e = self.locate(r).await?;
        let (x, y) = self.center(&e).await?;
        let covered = || {
            Error::Element(format!(
                "another element is on top of '{}' ({}) at the click point, so the approved click was NOT made",
                e.name, e.role
            ))
        };
        // Hit test BEFORE any mouse event (a move would already fire the covering element's
        // hover handlers), with browser-side DOM.* calls only.
        if guard.batch && !self.click_lands_on(&e, x, y).await {
            return Err(covered());
        }
        self.mouse("mouseMoved", x, y, json!({})).await?;
        self.check_input_origin(&e, expected).await?;
        for n in 1..=clicks {
            // Again right before every press (the move above may have run page handlers that
            // laid something over the point); on a mismatch neither press nor release is sent.
            if guard.batch && !self.click_lands_on(&e, x, y).await {
                return Err(covered());
            }
            let btn = json!({"button": "left", "clickCount": n});
            self.mouse("mousePressed", x, y, btn.clone()).await?;
            self.mouse("mouseReleased", x, y, btn).await?;
        }
        Ok(())
    }

    async fn focus_entry(&self, e: &RefEntry) -> Result<()> {
        self.cmd_on(
            e.session_id.as_deref(),
            "DOM.focus",
            json!({"backendNodeId": e.backend_id}),
        )
        .await
        .map_err(|err| Error::Element(format!("cannot focus: {err}")))
        .map(|_| ())
    }

    async fn key_inner(&self, spec: &str) -> Result<()> {
        self.key_inner_with_origin(spec, None, &StepGuard::default())
            .await
    }

    /// The node that has keyboard focus: `None` when nothing does (or it cannot be told
    /// apart from a frame / shadow host, which never equals an approved node).
    async fn focused_node(&self) -> Result<Option<(Option<String>, i64)>> {
        match self.focused_entry().await {
            Ok(f) => Ok(Some((f.session_id, f.backend_id))),
            Err(Error::Element(m)) if m == NO_FOCUS => match self.active_element().await {
                Ok(ActiveElement::Node {
                    backend_id,
                    hosts_other_document: false,
                }) => Ok(Some((None, backend_id))),
                _ => Ok(None),
            },
            Err(e) => Err(e),
        }
    }

    /// A batch key that activates whatever has focus (Enter, Space) needs focus to be on
    /// the node the previous step worked on. A page script can move focus to another
    /// control between steps; origin and URL do not change when it does. `fill` and
    /// `select` do not move focus themselves, so when nothing at all has focus the
    /// approved node is focused first; focus on any OTHER element stops the batch.
    async fn check_batch_focus(&self, guard: &StepGuard) -> Result<()> {
        let moved = |why: &str| {
            Err(Error::Element(format!(
                "{why}; the key was NOT pressed (it would activate an element the approval did not name)"
            )))
        };
        let Some(r) = guard.focus_ref.as_deref() else {
            return moved("no earlier step of this batch worked on an element to press the key on");
        };
        let want = self.locate(r).await?;
        let key = (want.session_id.clone(), want.backend_id);
        let mut now = self.focused_node().await?;
        if now.is_none() {
            self.focus_entry(&want).await?;
            now = self.focused_node().await?;
        }
        if now.as_ref() != Some(&key) {
            return moved("keyboard focus is not on the element the previous step used");
        }
        // Enter in a password / one-time-code / card field submits it: still refused.
        self.refuse_if_sensitive_now(&want, "key press").await
    }

    async fn check_focused_origin(&self, expected: Option<&str>) -> Result<()> {
        if expected.is_some() {
            let e = self.focused_entry().await?;
            self.check_input_origin(&e, expected).await?;
        }
        Ok(())
    }

    async fn key_inner_with_origin(
        &self,
        spec: &str,
        expected: Option<&str>,
        guard: &StepGuard,
    ) -> Result<()> {
        let k = parse_key(spec)?;
        let mut down = json!({
            "type": if k.text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": k.key, "code": k.code, "windowsVirtualKeyCode": k.vk,
            "nativeVirtualKeyCode": k.vk, "modifiers": k.modifiers,
        });
        if let Some(t) = &k.text {
            down["text"] = json!(t);
        }
        self.check_focused_origin(expected).await?;
        if guard.batch && is_activating_spec(spec) {
            self.check_batch_focus(guard).await?;
        }
        if k.text.is_some() && !is_navigation_spec(spec) {
            // Printable input: the focused element must still not be a secret field.
            match self.focused_entry().await {
                Ok(focused) => self.refuse_if_sensitive_now(&focused, "key press").await?,
                // `:focus` does not match on a page without OS focus: ask the document itself.
                Err(Error::Element(m)) if m == NO_FOCUS => {
                    self.refuse_if_active_element_secret().await?;
                }
                Err(e) => return Err(e),
            }
        }
        self.cmd("Input.dispatchKeyEvent", down).await?;
        self.check_focused_origin(expected).await?;
        self.cmd(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "key": k.key, "code": k.code,
                   "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk,
                   "modifiers": k.modifiers}),
        )
        .await
        .map(|_| ())
    }

    // -- public actions -------------------------------------------------------------------------

    /// Click an element with explicit dialog handling options.
    pub async fn click_with(&self, r: &str, opts: &DialogOptions) -> Result<ActionResult> {
        self.click_with_origin(r, opts, None).await
    }

    /// Click with a receiving-frame grant check.
    pub async fn click_with_origin(
        &self,
        r: &str,
        opts: &DialogOptions,
        expected: Option<&str>,
    ) -> Result<ActionResult> {
        self.click_with_guard(r, opts, expected, &StepGuard::default())
            .await
    }

    /// Click with a receiving-frame grant check and the extra checks of a batch step.
    pub async fn click_with_guard(
        &self,
        r: &str,
        opts: &DialogOptions,
        expected: Option<&str>,
        guard: &StepGuard,
    ) -> Result<ActionResult> {
        let before = self.baseline_with(opts).await?;
        let res = self.click_inner(r, 1, expected, guard).await;
        self.finish(before, res).await
    }

    /// Click an element: scroll into view, compute the box-model center, dispatch mouse events.
    pub async fn click(&self, r: &str) -> Result<ActionResult> {
        self.click_with(r, &DialogOptions::default()).await
    }

    /// Double-click an element.
    pub async fn double_click(&self, r: &str) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = self.click_inner(r, 2, None, &StepGuard::default()).await;
        self.finish(before, res).await
    }

    /// Move the mouse over an element.
    pub async fn hover(&self, r: &str) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let e = self.locate(r).await?;
            let (x, y) = self.center(&e).await?;
            self.mouse("mouseMoved", x, y, json!({})).await
        }
        .await;
        self.finish(before, res).await
    }

    /// Replace the content of an input/textarea/contenteditable with `text`.
    pub async fn fill(&self, r: &str, text: &str) -> Result<ActionResult> {
        self.fill_with_origin(r, text, None).await
    }

    /// Node-bound fill with an execution-time grant check. Focus theft cannot redirect it.
    pub async fn fill_with_origin(
        &self,
        r: &str,
        text: &str,
        expected: Option<&str>,
    ) -> Result<ActionResult> {
        self.fill_checked(
            r,
            text,
            |origin| expected.is_none_or(|e| grant_origin_matches(origin.as_deref(), e)),
            true,
        )
        .await
    }

    /// Type a secret value into a field, but only into a frame whose origin
    /// `accept` approves. The frame origin is read from the browser (not from
    /// page script) twice, before the field is focused and again immediately
    /// before the value is inserted; a different origin or document in between,
    /// or a navigation, types nothing. The value is never put into an error.
    pub async fn fill_secret<F>(&self, r: &str, text: &str, accept: F) -> Result<ActionResult>
    where
        F: Fn(Option<String>) -> bool + Send + Sync,
    {
        self.fill_checked(r, text, accept, false).await
    }

    /// Shared fill step. `refuse_sensitive` (plain, non-secret fills) re-reads the target's
    /// type / name / autocomplete from the live DOM right before writing, so a page that turned
    /// the field into a password input after the earlier check and the approval cannot get
    /// text typed into it. Unknown counts as sensitive.
    async fn fill_checked<F>(
        &self,
        r: &str,
        text: &str,
        accept: F,
        refuse_sensitive: bool,
    ) -> Result<ActionResult>
    where
        F: Fn(Option<String>) -> bool + Send + Sync,
    {
        let before = self.baseline().await?;
        let res = async {
            let e = self.locate(r).await?;
            if refuse_sensitive {
                self.refuse_if_sensitive_now(&e, "fill").await?;
            }
            let nav = self.shared.nav_gen.load(Ordering::SeqCst);
            let first = self.frame_identity(&e).await?;
            if !accept(first.origin.clone()) {
                return Err(Error::Element(
                    "refused: the frame that holds this field is not an origin the owner allowed for this secret; nothing was typed"
                        .into(),
                ));
            }
            let second = self.frame_identity(&e).await?;
            if refuse_sensitive {
                self.refuse_if_sensitive_now(&e, "fill").await?;
            }
            if second != first || self.shared.nav_gen.load(Ordering::SeqCst) != nav {
                return Err(Error::Element(
                    "the field's frame changed while it was being prepared; nothing was typed"
                        .into(),
                ));
            }
            // Written THROUGH the approved node itself (resolved by its backend
            // id in its own frame), never with tab-wide `Input.insertText`, so a
            // frame that steals focus cannot receive the value.
            let status = self
                .call_on(
                    &e,
                    "function(v){ if (!this.isConnected) return 'detached';                      const p = this instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype                        : this instanceof HTMLInputElement ? HTMLInputElement.prototype : null;                      const fire = () => { this.dispatchEvent(new Event('input', {bubbles: true}));                        this.dispatchEvent(new Event('change', {bubbles: true})); };                      if (p) { const d = Object.getOwnPropertyDescriptor(p, 'value');                        if (!d || !d.set) return 'unsupported'; d.set.call(this, v); fire();                        return this.value === v ? 'ok' : 'nostick'; }                      if (this.isContentEditable) { this.textContent = v; fire();                        return this.textContent === v ? 'ok' : 'nostick'; }                      return 'unsupported'; }",
                    &[json!(text)],
                )
                .await?;
            match status.as_str() {
                Some("ok") => Ok(()),
                Some("detached") => Err(Error::Element(
                    "the field is no longer attached to the page; nothing was typed".into(),
                )),
                Some("nostick") => Err(Error::Element(
                    "the page did not accept the value (it may only take real key events); nothing was typed with the keyboard instead"
                        .into(),
                )),
                _ => Err(Error::Element(
                    "this kind of field cannot be filled with a secret".into(),
                )),
            }
        }
        .await;
        self.finish(before, res).await
    }

    /// Type text key by key into an element (appends; use [`Page::fill`] to replace).
    pub async fn type_text(&self, r: &str, text: &str) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let e = self.locate(r).await?;
            // Live check, so no caller can type into a secret field through this path.
            self.refuse_if_sensitive_now(&e, "typing").await?;
            self.focus_entry(&e).await?;
            for ch in text.chars() {
                if ch == '\n' {
                    self.key_inner("Enter").await?;
                } else {
                    let s = ch.to_string();
                    self.cmd(
                        "Input.dispatchKeyEvent",
                        json!({"type": "keyDown", "text": s, "key": s}),
                    )
                    .await?;
                    self.cmd("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": s}))
                        .await?;
                }
            }
            Ok(())
        }
        .await;
        self.finish(before, res).await
    }

    /// Choose an `<option>` of a `<select>` by value or visible label.
    pub async fn select_option(&self, r: &str, value: &str) -> Result<ActionResult> {
        self.select_option_with_origin(r, value, None).await
    }

    /// Select only while the receiving frame still matches the grant.
    pub async fn select_option_with_origin(
        &self,
        r: &str,
        value: &str,
        expected: Option<&str>,
    ) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let e = self.locate(r).await?;
            self.check_input_origin(&e, expected).await?;
            let ok = self
                .call_on(
                    &e,
                    "function(v){ if (this.tagName !== 'SELECT') return false; \
                     for (const o of this.options) { if (o.value === v || o.text.trim() === v) { \
                     this.value = o.value; \
                     this.dispatchEvent(new Event('input', {bubbles: true})); \
                     this.dispatchEvent(new Event('change', {bubbles: true})); return true; } } \
                     return false; }",
                    &[json!(value)],
                )
                .await?;
            if ok.as_bool() == Some(true) {
                Ok(())
            } else {
                // The requested text is not echoed: the model already has it, and a result is
                // also shown in logs and later turns.
                Err(Error::Element(format!(
                    "no <option> matches the {}-character value; take a browser_snapshot to see the options",
                    value.chars().count()
                )))
            }
        }
        .await;
        self.finish(before, res).await
    }

    /// Press a key (`Enter`, `Tab`, `Control+a`, ...) on the focused element.
    pub async fn press_key(&self, key: &str) -> Result<ActionResult> {
        self.press_key_with_origin(key, None).await
    }

    /// Check the focused frame immediately before dispatching keyboard input.
    pub async fn press_key_with_origin(
        &self,
        key: &str,
        expected: Option<&str>,
    ) -> Result<ActionResult> {
        self.press_key_with_guard(key, expected, &StepGuard::default())
            .await
    }

    /// Press a key with the extra checks of a batch step.
    pub async fn press_key_with_guard(
        &self,
        key: &str,
        expected: Option<&str>,
        guard: &StepGuard,
    ) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = self.key_inner_with_origin(key, expected, guard).await;
        self.finish(before, res).await
    }

    /// Scroll with the mouse wheel over an element, or the viewport center when `r` is `None`.
    pub async fn scroll(&self, r: Option<&str>, dx: f64, dy: f64) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let (x, y) = if let Some(r) = r {
                let e = self.locate(r).await?;
                self.center(&e).await?
            } else {
                let m = self.cmd("Page.getLayoutMetrics", json!({})).await?;
                (
                    m["cssVisualViewport"]["clientWidth"]
                        .as_f64()
                        .unwrap_or(800.0)
                        / 2.0,
                    m["cssVisualViewport"]["clientHeight"]
                        .as_f64()
                        .unwrap_or(600.0)
                        / 2.0,
                )
            };
            self.mouse("mouseWheel", x, y, json!({"deltaX": dx, "deltaY": dy}))
                .await
        }
        .await;
        self.finish(before, res).await
    }

    /// Set the files of an `<input type=file>`.
    pub async fn upload_file(&self, r: &str, files: &[PathBuf]) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let e = self.locate(r).await?;
            let paths: Vec<String> = files
                .iter()
                .map(|p| {
                    std::fs::canonicalize(p)
                        .unwrap_or_else(|_| p.clone())
                        .to_string_lossy()
                        .trim_start_matches(r"\\?\")
                        .to_string()
                })
                .collect();
            self.cmd_on(
                e.session_id.as_deref(),
                "DOM.setFileInputFiles",
                json!({"files": paths, "backendNodeId": e.backend_id}),
            )
            .await
            .map(|_| ())
        }
        .await;
        self.finish(before, res).await
    }

    /// Wait for a condition (polling) up to `timeout` (default 30 s).
    pub async fn wait_for(
        &self,
        cond: &WaitCondition,
        timeout: Option<Duration>,
    ) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let timeout = timeout.unwrap_or(DEFAULT_WAIT);
        let res = match cond {
            WaitCondition::NetworkIdle => {
                self.wait_network_idle(Duration::from_millis(500), timeout)
                    .await
            }
            WaitCondition::Selector(sel) => {
                let js = format!("!!document.querySelector({})", json!(sel));
                self.poll_true(&js, timeout, "selector").await
            }
            WaitCondition::Text(t) => {
                let js = format!(
                    "(document.body && document.body.innerText || '').includes({})",
                    json!(t)
                );
                self.poll_true(&js, timeout, "text").await
            }
        };
        // The wanted condition already holds (or timed out): snapshot only, no settle.
        self.finish_with(before, res, false).await
    }

    async fn poll_true(&self, js: &str, timeout: Duration, what: &str) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(v) = self.evaluate(js).await
                && v.as_bool() == Some(true)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!("waiting for {what}")));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Like [`Page::finish`] for a navigation. A page whose `load` event does not
    /// arrive in [`NAVIGATION_LOAD_DEADLINE`] (an ad or tracker that never
    /// answers) is usually already readable: the model gets the page as it is
    /// with a note saying it was still loading, instead of a bare timeout that
    /// leaves it blind.
    async fn finish_navigation(
        &self,
        before: Snapshot,
        outcome: Result<()>,
    ) -> Result<ActionResult> {
        let still_loading = matches!(&outcome, Err(Error::Timeout(m)) if m == "page load");
        let mut result = self
            .finish(before, if still_loading { Ok(()) } else { outcome })
            .await?;
        if still_loading {
            result.settle_note = Some(format!(
                "the page was still loading after {}s; this is what is on it now. Read it, or call browser_wait / browser_snapshot to look again; do not reopen the same address in a loop",
                NAVIGATION_LOAD_DEADLINE.as_secs()
            ));
        }
        Ok(result)
    }

    async fn navigate_inner(&self, url: &str) -> Result<()> {
        let mut rx = self.client.subscribe();
        let r = self.cmd("Page.navigate", json!({"url": url})).await?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            return Err(Error::Navigation(format!("{url}: {err}")));
        }
        if r.get("loaderId").is_some() {
            self.wait_load(&mut rx, NAVIGATION_LOAD_DEADLINE).await?;
        }
        Ok(())
    }

    /// Navigate and return the first view of the page with snapshot and dialog options.
    pub async fn navigate_snapshot_with(
        &self,
        url: &str,
        opts: &crate::snapshot::SnapshotOptions,
        dialog_opts: &DialogOptions,
    ) -> Result<ActionResult> {
        if let Ok(mut d) = self.shared.dialogs.lock() {
            d.clear();
        }
        if let Ok(mut p) = self.shared.dialog_policy.lock() {
            *p = dialog_opts.clone();
        }
        self.set_snapshot_options(opts).await;
        if let Ok(mut m) = self.shared.action_mark.lock() {
            *m = Some((Instant::now(), self.shared.nav_gen.load(Ordering::SeqCst)));
        }
        let res = self.navigate_inner(url).await;
        self.finish_navigation(Snapshot::default(), res).await
    }

    /// Navigate and return the first view of the page: one snapshot taken with `opts` (no
    /// baseline snapshot, so the diff is against an empty page). Used by `browser_open`.
    pub async fn navigate_snapshot(
        &self,
        url: &str,
        opts: &crate::snapshot::SnapshotOptions,
    ) -> Result<ActionResult> {
        self.navigate_snapshot_with(url, opts, &DialogOptions::default())
            .await
    }

    /// Navigate to a URL with explicit dialog handling options.
    pub async fn navigate_with(&self, url: &str, opts: &DialogOptions) -> Result<ActionResult> {
        let before = self.baseline_with(opts).await?;
        let res = self.navigate_inner(url).await;
        self.finish_navigation(before, res).await
    }

    /// Navigate to a URL and wait for the load event.
    pub async fn navigate(&self, url: &str) -> Result<ActionResult> {
        self.navigate_with(url, &DialogOptions::default()).await
    }

    async fn history_inner(&self, delta: i64) -> Result<()> {
        let h = self.cmd("Page.getNavigationHistory", json!({})).await?;
        let cur = h["currentIndex"].as_i64().unwrap_or(0);
        let entries = h["entries"].as_array().cloned().unwrap_or_default();
        let target = usize::try_from(cur + delta)
            .ok()
            .and_then(|i| entries.get(i))
            .ok_or_else(|| Error::Navigation("no history entry in that direction".into()))?;
        let mut rx = self.client.subscribe();
        self.cmd(
            "Page.navigateToHistoryEntry",
            json!({"entryId": target["id"]}),
        )
        .await?;
        // Same-document entries fire `navigatedWithinDocument` instead of a load event.
        let _ = self
            .wait_events(
                &mut rx,
                &["Page.loadEventFired", "Page.navigatedWithinDocument"],
                Duration::from_secs(5),
            )
            .await;
        Ok(())
    }

    /// Go back in history with explicit dialog handling options.
    pub async fn back_with(&self, opts: &DialogOptions) -> Result<ActionResult> {
        let before = self.baseline_with(opts).await?;
        let res = self.history_inner(-1).await;
        self.finish(before, res).await
    }

    /// Go back in history.
    pub async fn back(&self) -> Result<ActionResult> {
        self.back_with(&DialogOptions::default()).await
    }

    /// Go forward in history.
    pub async fn forward(&self) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = self.history_inner(1).await;
        self.finish(before, res).await
    }

    /// Capture a screenshot (PNG, or JPEG with quality) of the viewport, full page, or an element.
    #[allow(clippy::many_single_char_names)]
    pub async fn screenshot(&self, opts: &ScreenshotOptions) -> Result<Vec<u8>> {
        let mut p = json!({"fromSurface": true});
        match opts.jpeg_quality {
            Some(q) => {
                p["format"] = json!("jpeg");
                p["quality"] = json!(q.clamp(1, 100));
            }
            None => p["format"] = json!("png"),
        }
        match &opts.target {
            ScreenshotTarget::Viewport => {}
            ScreenshotTarget::FullPage => {
                let m = self.cmd("Page.getLayoutMetrics", json!({})).await?;
                let size = &m["cssContentSize"];
                p["captureBeyondViewport"] = json!(true);
                p["clip"] = json!({"x": 0, "y": 0, "scale": 1,
                    "width": size["width"].as_f64().unwrap_or(1280.0),
                    "height": size["height"].as_f64().unwrap_or(800.0)});
            }
            ScreenshotTarget::Element(r) => {
                let e = self.locate(r).await?;
                self.center(&e).await?; // scrolls into view
                let q = self
                    .quad(e.session_id.as_deref(), e.backend_id, "border")
                    .await?;
                let (ox, oy) = self.frame_offset(&e.frame_id).await;
                let m = self.cmd("Page.getLayoutMetrics", json!({})).await?;
                let px = m["cssVisualViewport"]["pageX"].as_f64().unwrap_or(0.0);
                let py = m["cssVisualViewport"]["pageY"].as_f64().unwrap_or(0.0);
                let xs = q.iter().step_by(2).copied();
                let ys = q.iter().skip(1).step_by(2).copied();
                let (x0, x1) = xs.fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
                let (y0, y1) = ys.fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
                p["captureBeyondViewport"] = json!(true);
                p["clip"] = json!({"x": x0 + ox + px, "y": y0 + oy + py, "scale": 1,
                    "width": x1 - x0, "height": y1 - y0});
            }
        }
        let r = self.cmd("Page.captureScreenshot", p).await?;
        let data = r["data"]
            .as_str()
            .ok_or_else(|| Error::Json("captureScreenshot returned no data".into()))?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| Error::Json(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys() {
        let k = parse_key("Enter").unwrap();
        assert_eq!(
            (k.key.as_str(), k.vk, k.text.as_deref()),
            ("Enter", 13, Some("\r"))
        );
        let k = parse_key("ArrowDown").unwrap();
        assert_eq!((k.code.as_str(), k.vk, k.text), ("ArrowDown", 40, None));
        assert!(parse_key("Bogus").is_err());
    }

    #[test]
    fn character_keys_and_modifiers() {
        let k = parse_key("a").unwrap();
        assert_eq!((k.key.as_str(), k.code.as_str(), k.vk), ("a", "KeyA", 65));
        assert_eq!(k.text.as_deref(), Some("a"));
        let k = parse_key("Control+a").unwrap();
        assert_eq!(k.modifiers, MOD_CTRL);
        assert!(k.text.is_none(), "ctrl combos must not insert text");
        let k = parse_key("Shift+Tab").unwrap();
        assert_eq!((k.key.as_str(), k.modifiers), ("Tab", MOD_SHIFT));
        let k = parse_key("Shift+a").unwrap();
        assert_eq!(k.key, "A");
        assert!(parse_key("Hyper+a").is_err());
    }

    #[test]
    fn quad_center_is_average() {
        let c = quad_center(&[0.0, 0.0, 10.0, 0.0, 10.0, 20.0, 0.0, 20.0]).unwrap();
        assert!((c.0 - 5.0).abs() < f64::EPSILON && (c.1 - 10.0).abs() < f64::EPSILON);
        assert!(quad_center(&[1.0]).is_none());
    }

    #[test]
    fn grant_origin_matches_canonicalizes_ports_trailing_dots_and_case() {
        let cases = [
            (Some("https://example.com"), "https://example.com", true),
            (
                Some("https://Example.COM."),
                "https://example.com:443",
                true,
            ),
            (Some("http://example.com:80"), "http://example.com", true),
            (
                Some("https://example.com/login?a=1"),
                "https://example.com",
                true,
            ),
            (Some("https://[::1]:8443"), "https://[::1]:8443", true),
            (
                Some("https://xn--bcher-kva.example"),
                "https://XN--BCHER-KVA.EXAMPLE.",
                true,
            ),
        ];
        for (origin, expected, want) in cases {
            assert_eq!(
                grant_origin_matches(origin, expected),
                want,
                "origin={origin:?} expected={expected:?}"
            );
        }
    }

    #[test]
    fn grant_origin_matches_rejects_opaque_scheme_port_and_host_mismatches() {
        let cases = [
            (None, "https://example.com"),
            (Some(""), "https://example.com"),
            (Some("null"), "https://example.com"),
            (Some("javascript:alert(1)"), "https://example.com"),
            (Some("file:///etc/passwd"), "file:///etc/passwd"),
            (Some("http://example.com"), "https://example.com"),
            (Some("https://example.com:8443"), "https://example.com"),
            (Some("https://sub.example.com"), "https://example.com"),
            (Some("https://example.com"), "not-a-url"),
        ];
        for (origin, expected) in cases {
            assert!(
                !grant_origin_matches(origin, expected),
                "origin={origin:?} expected={expected:?} must not match"
            );
        }
    }

    #[test]
    fn web_origin_accepts_clean_http_origins_and_rejects_opaque_or_paths() {
        let accept = [
            (Some("https://example.com"), Some("https://example.com")),
            (
                Some("  http://localhost:8080  "),
                Some("http://localhost:8080"),
            ),
            (Some("https://[::1]:8443"), Some("https://[::1]:8443")),
        ];
        for (raw, expected) in accept {
            assert_eq!(web_origin(raw).as_deref(), expected, "raw={raw:?}");
        }

        let reject = [
            None,
            Some(""),
            Some("null"),
            Some("https://"),
            Some("http://"),
            Some("https://example.com/path"),
            Some("https://example.com?q=1"),
            Some("https://example.com#frag"),
            Some("https://user@example.com"),
            Some("https://example.com extra"),
            Some("ws://example.com"),
            Some("file:///etc/passwd"),
        ];
        for raw in reject {
            assert_eq!(web_origin(raw), None, "raw={raw:?} must be rejected");
        }
    }
}
