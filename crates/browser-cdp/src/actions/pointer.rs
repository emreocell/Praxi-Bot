//! Coordinate-based pointer actions: the fallback for what has no element ref.
//!
//! A cross-origin "are you human" checkbox, a canvas, a custom widget drawn with `<div>`s
//! and no role: the accessibility snapshot has nothing to click, so the model looks at a
//! screenshot and names a point. Everything here works in **screenshot pixels**: the
//! picture [`Page::grounded_screenshot`] returns is the viewport scaled by
//! [`pointer_scale`], and every pointer action maps its point back with the same scale, so
//! the model never has to know about device pixel ratios or page zoom.
//!
//! The usual safety rules still hold: typing refuses a focused secret field and a focused
//! element in another frame, the receiving frame's origin is checked when an approval
//! named one, and a screenshot refuses while a typed one-time code is on screen.

use std::fmt::Write as _;

use base64::Engine as _;
use serde_json::{Value, json};

use super::{
    ActionResult, DialogOptions, Feel, Jitter, NO_FOCUS, StepGuard, grant_origin_matches, human,
    sleep, web_origin,
};
use crate::error::{Error, Result};
use crate::page::Page;
use crate::snapshot::RefEntry;

/// Widest screenshot a model gets, in pixels: wider viewports are scaled down to this.
pub const POINTER_MAX_EDGE: f64 = 1280.0;

/// Screenshot pixels per CSS pixel for a viewport `css_width` wide. Pure, so the screenshot
/// and the pointer action that follows it agree without sharing any state.
#[must_use]
pub fn pointer_scale(css_width: f64) -> f64 {
    if css_width > POINTER_MAX_EDGE {
        POINTER_MAX_EDGE / css_width
    } else {
        1.0
    }
}

/// Which mouse button a click uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    /// The main button.
    Left,
    /// The context-menu button.
    Right,
}

impl PointerButton {
    fn cdp(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// What the pointer does at (or from) the point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerOp {
    /// Press and release `count` times (2 = double click).
    Click {
        /// The button.
        button: PointerButton,
        /// How many presses in a row.
        count: u32,
    },
    /// Move there and rest (hover menus, tooltips).
    Move,
    /// Press at the point, move to `to`, release.
    Drag {
        /// Where the button is released, in screenshot pixels.
        to: (f64, f64),
    },
    /// Turn the mouse wheel over the point; deltas are CSS pixels (positive: down / right).
    Wheel {
        /// Pixels right.
        dx: f64,
        /// Pixels down.
        dy: f64,
    },
}

/// One pointer action.
#[derive(Debug, Clone, Copy)]
pub struct PointerRequest<'a> {
    /// What to do.
    pub op: PointerOp,
    /// The point, in screenshot pixels.
    pub at: (f64, f64),
    /// How the pointer travels.
    pub feel: Feel,
    /// The origin an approval named for the receiving frame, if any.
    pub expected: Option<&'a str>,
}

/// The viewport as the pointer sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerView {
    /// Viewport width in CSS pixels.
    pub css_width: f64,
    /// Viewport height in CSS pixels.
    pub css_height: f64,
    /// Where the visible part starts on the page (CSS pixels).
    pub page_x: f64,
    /// Where the visible part starts on the page (CSS pixels).
    pub page_y: f64,
}

impl PointerView {
    /// Screenshot pixels per CSS pixel.
    #[must_use]
    pub fn scale(&self) -> f64 {
        pointer_scale(self.css_width)
    }

    /// Screenshot size in pixels.
    #[must_use]
    pub fn image_size(&self) -> (u32, u32) {
        let s = self.scale();
        (
            (self.css_width * s).round().max(1.0) as u32,
            (self.css_height * s).round().max(1.0) as u32,
        )
    }

    /// A screenshot point as a CSS viewport point; an error when it is off the picture.
    pub fn to_css(&self, at: (f64, f64)) -> Result<(f64, f64)> {
        let (width, height) = self.image_size();
        let (px, py) = at;
        let inside = px.is_finite()
            && py.is_finite()
            && px >= 0.0
            && py >= 0.0
            && px < f64::from(width)
            && py < f64::from(height);
        if !inside {
            return Err(Error::Element(format!(
                "the point ({px:.0}, {py:.0}) is outside the screenshot, which is {width}x{height} pixels; take a new screenshot and pick a point inside it"
            )));
        }
        let scale = self.scale();
        Ok((px / scale, py / scale))
    }
}

/// A box on the screenshot worth knowing about before clicking.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// `iframe`, `canvas`, `embed` or `object`.
    pub kind: String,
    /// Short label: a frame's source host, a title.
    pub label: String,
    /// Left edge, screenshot pixels.
    pub x: i32,
    /// Top edge, screenshot pixels.
    pub y: i32,
    /// Width, screenshot pixels.
    pub w: i32,
    /// Height, screenshot pixels.
    pub h: i32,
}

/// A screenshot made for pointing at.
#[derive(Debug, Clone)]
pub struct Grounded {
    /// JPEG bytes.
    pub bytes: Vec<u8>,
    /// Picture size in pixels.
    pub image: (u32, u32),
    /// The viewport it shows.
    pub view: PointerView,
    /// Frames and canvases on screen (what the accessibility snapshot cannot click into).
    pub regions: Vec<Region>,
}

impl Grounded {
    /// The lines that tell the model how to use the picture.
    #[must_use]
    pub fn describe(&self) -> String {
        let (w, h) = self.image;
        let mut out = format!(
            "image {w}x{h} px = the visible part of the page; browser_pointer takes x,y in these pixels (0,0 is the top-left corner)"
        );
        if !self.regions.is_empty() {
            out.push_str("\nframes and canvases on screen (the page text cannot reach into these; click inside them by pointer):");
            for r in &self.regions {
                let _ = write!(
                    out,
                    "\n- {} {:?} at x={} y={} w={} h={} (centre {},{})",
                    r.kind,
                    r.label,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    r.x + r.w / 2,
                    r.y + r.h / 2
                );
            }
        }
        out
    }
}

/// Printable page text made safe for one result line.
fn clean(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut t: String = flat.chars().take(max).collect();
    if flat.chars().count() > max {
        t.push('\u{2026}');
    }
    t.replace('"', "'")
}

/// Most frames / canvases listed.
const MAX_REGIONS: usize = 12;

impl Page {
    /// The viewport in CSS pixels.
    pub async fn pointer_view(&self) -> Result<PointerView> {
        let m = self.cmd("Page.getLayoutMetrics", json!({})).await?;
        let v = &m["cssVisualViewport"];
        let get = |k: &str| v[k].as_f64().unwrap_or(0.0);
        let (w, h) = (get("clientWidth"), get("clientHeight"));
        if w < 1.0 || h < 1.0 {
            return Err(Error::Element("the page has no visible area".into()));
        }
        Ok(PointerView {
            css_width: w,
            css_height: h,
            page_x: get("pageX"),
            page_y: get("pageY"),
        })
    }

    /// Frames and canvases on the visible part of the top document, as screenshot boxes.
    async fn pointer_regions(&self, view: &PointerView) -> Vec<Region> {
        let Ok(doc) = self.cmd("DOM.getDocument", json!({"depth": 0})).await else {
            return Vec::new();
        };
        let Ok(found) = self
            .cmd(
                "DOM.querySelectorAll",
                json!({"nodeId": doc["root"]["nodeId"], "selector": "iframe,canvas,embed,object"}),
            )
            .await
        else {
            return Vec::new();
        };
        let s = view.scale();
        let mut out = Vec::new();
        for id in found["nodeIds"].as_array().into_iter().flatten() {
            if out.len() >= MAX_REGIONS {
                break;
            }
            let Ok(boxed) = self.cmd("DOM.getBoxModel", json!({"nodeId": id})).await else {
                continue;
            };
            let q: Vec<f64> = boxed["model"]["border"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            if q.len() < 8 {
                continue;
            }
            let (x0, x1) = (
                q[0].min(q[2]).min(q[4]).min(q[6]),
                q[0].max(q[2]).max(q[4]).max(q[6]),
            );
            let (y0, y1) = (
                q[1].min(q[3]).min(q[5]).min(q[7]),
                q[1].max(q[3]).max(q[5]).max(q[7]),
            );
            // Fully off screen: not clickable now.
            if x1 <= 0.0 || y1 <= 0.0 || x0 >= view.css_width || y0 >= view.css_height {
                continue;
            }
            let Ok(d) = self.cmd("DOM.describeNode", json!({"nodeId": id})).await else {
                continue;
            };
            let node = &d["node"];
            let attrs: Vec<&str> = node["attributes"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("")).collect())
                .unwrap_or_default();
            let attr = |k: &str| {
                attrs
                    .chunks(2)
                    .find(|p| p.len() == 2 && p[0].eq_ignore_ascii_case(k))
                    .map(|p| p[1])
            };
            let label = attr("title")
                .filter(|t| !t.trim().is_empty())
                .map(|t| clean(t, 50))
                .or_else(|| {
                    attr("src")
                        .and_then(|u| url::Url::parse(u).ok())
                        .and_then(|u| u.host_str().map(str::to_owned))
                })
                .unwrap_or_default();
            out.push(Region {
                kind: node["nodeName"]
                    .as_str()
                    .unwrap_or("frame")
                    .to_ascii_lowercase(),
                label,
                x: (x0.max(0.0) * s).round() as i32,
                y: (y0.max(0.0) * s).round() as i32,
                w: ((x1.min(view.css_width) - x0.max(0.0)) * s).round() as i32,
                h: ((y1.min(view.css_height) - y0.max(0.0)) * s).round() as i32,
            });
        }
        out
    }

    /// A JPEG of what is on screen, scaled by [`pointer_scale`], with the frames and canvases
    /// that cannot be reached by ref. Refuses like [`Page::screenshot`] while a one-time code
    /// is on screen.
    pub async fn grounded_screenshot(&self) -> Result<Grounded> {
        self.refuse_secret_capture()?;
        let view = self.pointer_view().await?;
        let s = view.scale();
        let r = self
            .cmd(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 70, "fromSurface": true,
                       "clip": {"x": view.page_x, "y": view.page_y,
                                "width": view.css_width, "height": view.css_height, "scale": s}}),
            )
            .await?;
        self.refuse_secret_capture()?;
        let data = r["data"]
            .as_str()
            .ok_or_else(|| Error::Json("captureScreenshot returned no data".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| Error::Json(e.to_string()))?;
        let regions = self.pointer_regions(&view).await;
        Ok(Grounded {
            bytes,
            image: view.image_size(),
            view,
            regions,
        })
    }

    /// What is under a CSS viewport point, as the browser reports it (never page script): words
    /// for the model, and the origin that would receive an event there. A bare page background
    /// has no node of its own: the click still goes to the top document.
    ///
    /// The origin is the one of the frame that really receives the event. A point over an
    /// `<iframe>` hits the frame element of the TOP document, so the embedded frame is looked up
    /// by its own id, and every frame nested inside it counts too: the origin is `Some` only when
    /// all of them agree (an opaque or mixed stack is unknown, which no rule matches).
    async fn hit_at(&self, x: f64, y: f64) -> Result<Hit> {
        let background = || Hit {
            node: None,
            what: "nothing but the page background".to_owned(),
            origin: None,
            top: true,
        };
        let Ok(hit) = self
            .cmd(
                "DOM.getNodeForLocation",
                json!({"x": x.round() as i64, "y": y.round() as i64,
                       "includeUserAgentShadowDOM": false, "ignorePointerEventsNone": false}),
            )
            .await
        else {
            return Ok(background());
        };
        let Some(backend_id) = hit["backendNodeId"].as_i64() else {
            return Ok(background());
        };
        let hit_frame = hit["frameId"].as_str().unwrap_or_default().to_owned();
        let hit_frame_id = hit_frame.clone();
        let d = self
            .cmd("DOM.describeNode", json!({"backendNodeId": backend_id}))
            .await?;
        let node = &d["node"];
        let tag = node["nodeName"].as_str().unwrap_or("").to_ascii_lowercase();
        let attrs: Vec<&str> = node["attributes"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("")).collect())
            .unwrap_or_default();
        let attr = |k: &str| {
            attrs
                .chunks(2)
                .find(|p| p.len() == 2 && p[0].eq_ignore_ascii_case(k))
                .map(|p| p[1])
        };
        let mut what = format!("<{tag}");
        for k in ["type", "role"] {
            if let Some(v) = attr(k).filter(|v| !v.is_empty()) {
                let _ = write!(what, " {k}={}", clean(v, 20));
            }
        }
        what.push('>');
        if let Some(label) = ["aria-label", "title", "alt", "name", "id"]
            .iter()
            .find_map(|k| attr(k).filter(|v| !v.trim().is_empty()))
        {
            let _ = write!(what, " \"{}\"", clean(label, 60));
        }
        let embedded = node["frameId"].as_str().filter(|f| !f.is_empty());
        let is_frame = matches!(tag.as_str(), "iframe" | "frame");
        if is_frame {
            what.push_str(" (a frame: the click goes into it)");
        }
        let origin = match (embedded, is_frame) {
            (Some(child), _) => self.subtree_origin(child).await?,
            // A frame element with no frame behind it (not loaded yet): unknown, fail closed.
            (None, true) => None,
            (None, false) if hit_frame.is_empty() => self.top_origin().await,
            (None, false) => {
                self.frame_identity(&RefEntry {
                    session_id: None,
                    frame_id: hit_frame,
                    backend_id,
                    name: String::new(),
                    role: String::new(),
                    sensitive: false,
                })
                .await?
                .origin
            }
        };
        Ok(Hit {
            node: Some((backend_id, hit_frame_id)),
            what,
            origin,
            top: false,
        })
    }

    /// The origin of frame `root` when it and every frame inside it share one web origin;
    /// `None` when any is opaque or they differ. An error when `root` is not in the frame tree.
    async fn subtree_origin(&self, root: &str) -> Result<Option<String>> {
        let gone = || Error::Element("the frame under the point is gone".into());
        let top = self
            .cmd("Page.getFrameTree", json!({}))
            .await
            .map_err(|e| Error::Element(format!("cannot read the frames of the page: {e}")))?;
        let mut stack = vec![&top["frameTree"]];
        let mut found = None;
        while let Some(n) = stack.pop() {
            if n["frame"]["id"].as_str() == Some(root) {
                found = Some(n);
                break;
            }
            stack.extend(n["childFrames"].as_array().into_iter().flatten());
        }
        // A frame of another site lives in its own process: the top page's tree stops at it, and
        // the frame's own session reports what is inside.
        let own;
        let found = if let Some(n) = found {
            n
        } else {
            let session = self
                .shared
                .oopif
                .lock()
                .ok()
                .and_then(|m| m.get(root).cloned())
                .ok_or_else(gone)?;
            own = self
                .cmd_on(Some(&session), "Page.getFrameTree", json!({}))
                .await
                .map_err(|e| {
                    Error::Element(format!("cannot read the frame under the point: {e}"))
                })?;
            if own["frameTree"]["frame"]["id"].as_str() != Some(root) {
                return Err(gone());
            }
            &own["frameTree"]
        };
        let mut origins = Vec::new();
        let mut stack = vec![found];
        while let Some(n) = stack.pop() {
            origins.push(web_origin(n["frame"]["securityOrigin"].as_str()));
            stack.extend(n["childFrames"].as_array().into_iter().flatten());
        }
        let first = origins.first().cloned().flatten();
        Ok(first.filter(|f| origins.iter().all(|o| o.as_deref() == Some(f.as_str()))))
    }

    /// The origin that receives a pointer event at `hit`.
    async fn hit_origin(&self, hit: &Hit) -> Option<String> {
        if hit.top {
            self.top_origin().await
        } else {
            hit.origin.clone()
        }
    }

    /// Refuse when an approval named an origin and the one under the point is another (or unknown).
    async fn check_hit_origin(&self, hit: &Hit, expected: Option<&str>) -> Result<Option<String>> {
        let origin = self.hit_origin(hit).await;
        if let Some(expected) = expected
            && !grant_origin_matches(origin.as_deref(), expected)
        {
            return Err(Error::Element(
                "receiving frame origin changed; ask again before input".into(),
            ));
        }
        Ok(origin)
    }

    /// The point again after the pointer travelled there (a hover can open a menu, move a frame
    /// or navigate): the receiving origin must still be the one it was, and the approved one,
    /// and the element under the point must still be the one `before` saw.
    async fn recheck_hit(
        &self,
        (x, y): (f64, f64),
        before: &Hit,
        before_origin: Option<&str>,
        expected: Option<&str>,
    ) -> Result<()> {
        let hit = self.hit_at(x, y).await?;
        let now = self.check_hit_origin(&hit, expected).await?;
        if now.as_deref() != before_origin {
            return Err(Error::Element(
                "the page under the pointer changed while it moved; nothing was pressed. Take a new screenshot and look again"
                    .into(),
            ));
        }
        if let Some(why) = under_point_changed(before, &hit) {
            return Err(Error::Element(why));
        }
        Ok(())
    }

    /// Browser-reported origin that would receive the pointer events of an action at `at` (and,
    /// for a drag, released at `to`). `None`: unknown, opaque, or the two ends differ.
    pub async fn pointer_origin(
        &self,
        at: (f64, f64),
        to: Option<(f64, f64)>,
    ) -> Result<Option<String>> {
        let view = self.pointer_view().await?;
        let (x, y) = view.to_css(at)?;
        let first = self.hit_at(x, y).await?;
        let origin = self.hit_origin(&first).await;
        if let Some(to) = to {
            let (tx, ty) = view.to_css(to)?;
            let second = self.hit_at(tx, ty).await?;
            if self.hit_origin(&second).await != origin {
                return Ok(None);
            }
        }
        Ok(origin)
    }

    async fn glide_to(&self, to: (f64, f64), feel: Feel, rng: &mut Jitter) -> Result<()> {
        self.glide(to, feel, rng).await
    }

    async fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        self.mouse("mouseWheel", x, y, json!({"deltaX": dx, "deltaY": dy}))
            .await
    }

    async fn pointer_inner(&self, req: &PointerRequest<'_>, hit_text: &mut String) -> Result<()> {
        let view = self.pointer_view().await?;
        let (x, y) = view.to_css(req.at)?;
        let mut rng = Jitter::from_clock();
        let hit = self.hit_at(x, y).await?;
        hit_text.clone_from(&hit.what);
        // Every kind of pointer action is judged by the frame under the point (a hover or a wheel
        // reaches the page too), and a drag by where it is released as well.
        let origin = self.check_hit_origin(&hit, req.expected).await?;
        let drop_point = match req.op {
            PointerOp::Drag { to } => {
                let (tx, ty) = view.to_css(to)?;
                let end = self.hit_at(tx, ty).await?;
                let end_origin = self.check_hit_origin(&end, req.expected).await?;
                if end_origin != origin {
                    return Err(Error::Element(
                        "the drag would end in another frame than it starts in; nothing was pressed"
                            .into(),
                    ));
                }
                Some(((tx, ty), end))
            }
            _ => None,
        };
        match req.op {
            PointerOp::Move => {
                self.glide_to((x, y), req.feel, &mut rng).await?;
                sleep(human::hover_dwell(req.feel, &mut rng)).await;
            }
            PointerOp::Wheel { dx, dy } => {
                self.glide_to((x, y), req.feel, &mut rng).await?;
                self.recheck_hit((x, y), &hit, origin.as_deref(), req.expected)
                    .await?;
                self.wheel(x, y, dx, dy).await?;
                // Let smooth-scrolling pages move before the picture is taken.
                sleep(std::time::Duration::from_millis(150)).await;
            }
            PointerOp::Click { button, count } => {
                self.glide_to((x, y), req.feel, &mut rng).await?;
                sleep(human::settle_pause(req.feel, &mut rng)).await;
                // The mouse has arrived: what is under it now is what the press will hit.
                self.recheck_hit((x, y), &hit, origin.as_deref(), req.expected)
                    .await?;
                let (name, mask) = match button {
                    PointerButton::Left => (button.cdp(), 1),
                    PointerButton::Right => (button.cdp(), 2),
                };
                for n in 1..=count.clamp(1, 3) {
                    let p = json!({"button": name, "clickCount": n, "buttons": mask});
                    self.mouse("mousePressed", x, y, p).await?;
                    sleep(human::hold_pause(req.feel, &mut rng)).await;
                    let r = json!({"button": name, "clickCount": n, "buttons": 0});
                    self.mouse("mouseReleased", x, y, r).await?;
                }
            }
            PointerOp::Drag { .. } => {
                let ((tx, ty), end_hit) =
                    drop_point.ok_or_else(|| Error::Element("a drag needs an end point".into()))?;
                self.glide_to((x, y), req.feel, &mut rng).await?;
                sleep(human::settle_pause(req.feel, &mut rng)).await;
                self.recheck_hit((x, y), &hit, origin.as_deref(), req.expected)
                    .await?;
                self.mouse(
                    "mousePressed",
                    x,
                    y,
                    json!({"button": "left", "clickCount": 1, "buttons": 1}),
                )
                .await?;
                // The bot's own pointer carries the drag to where it is released.
                self.bot_cursor_move((tx, ty)).await;
                // Always several moves: a drag handler needs them to see a drag at all.
                let steps = 8;
                for i in 1..=steps {
                    let t = f64::from(i) / f64::from(steps);
                    self.mouse(
                        "mouseMoved",
                        x + (tx - x) * t,
                        y + (ty - y) * t,
                        json!({"button": "left", "buttons": 1}),
                    )
                    .await?;
                    sleep(human::step_pause(req.feel, &mut rng)).await;
                }
                // The button is down: release it where it was approved or not at all.
                self.recheck_hit((tx, ty), &end_hit, origin.as_deref(), req.expected)
                    .await?;
                self.mouse(
                    "mouseReleased",
                    tx,
                    ty,
                    json!({"button": "left", "clickCount": 1, "buttons": 0}),
                )
                .await?;
                if let Ok(mut p) = self.shared.pointer.lock() {
                    *p = Some((tx, ty));
                }
            }
        }
        Ok(())
    }

    /// Click, move, drag or wheel at a screenshot point. Returns the refreshed view and a
    /// line saying what the browser found under the point.
    pub async fn pointer(
        &self,
        req: &PointerRequest<'_>,
        opts: &DialogOptions,
    ) -> Result<(ActionResult, String)> {
        let before = self.baseline_with(opts).await?;
        let mut hit = String::new();
        let res = self.pointer_inner(req, &mut hit).await;
        let acted = self.finish(before, res).await?;
        Ok((acted, hit))
    }

    /// The node that has keyboard focus, after the checks typing needs: its frame is the one an
    /// approval named, and it is not a secret field (a frame or shadow host counts as one: it
    /// cannot be looked into). `None` when nothing has focus.
    async fn typing_target(&self, expected: Option<&str>) -> Result<Option<(Option<String>, i64)>> {
        match self.focused_entry().await {
            Ok(mut e) => {
                self.check_input_origin(&e, expected).await?;
                // Unknown until looked at: `refuse_if_sensitive_now` reads the live node.
                e.sensitive = true;
                self.refuse_if_sensitive_now(&e, "typed text").await?;
                Ok(Some((e.session_id, e.backend_id)))
            }
            Err(Error::Element(m)) if m == NO_FOCUS => {
                self.refuse_if_active_element_secret().await?;
                if let Some(expected) = expected
                    && !grant_origin_matches(self.top_origin().await.as_deref(), expected)
                {
                    return Err(Error::Element(
                        "receiving frame origin changed; ask again before input".into(),
                    ));
                }
                self.focused_node().await
            }
            Err(e) => Err(e),
        }
    }

    /// Type `text` into whatever has keyboard focus (after a pointer click put focus there).
    /// Refuses a focused secret field and a focused element in another frame or shadow
    /// host, the same way a key press does; the secret path is `fill` with a secret name.
    /// The focus is verified again before EVERY character: a page script that moves focus to a
    /// password field or another frame while the text goes in (a key handler, the Enter at a
    /// line break) stops the typing there, with the rest NOT typed.
    pub async fn pointer_type(&self, text: &str, expected: Option<&str>) -> Result<ActionResult> {
        let before = self.baseline().await?;
        let res = async {
            let start = self.typing_target(expected).await?;
            for ch in text.chars() {
                if self.typing_target(expected).await? != start {
                    return Err(Error::Element(
                        "refused: keyboard focus moved while typing, so the rest was NOT typed. Click the field again and check the page"
                            .into(),
                    ));
                }
                if ch == '\n' {
                    self.key_inner_with_origin("Enter", expected, &StepGuard::default())
                        .await?;
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
}

/// What a pointer point lands on.
struct Hit {
    /// The node and frame the browser reported there (`None`: nothing but the page background),
    /// so the same spot can be recognised again after the pointer has travelled to it.
    node: Option<(i64, String)>,
    /// Words for the model.
    what: String,
    /// Origin of the receiving frame (see [`Page::hit_at`]); unused when `top`.
    origin: Option<String>,
    /// Nothing but the page background: the top document receives it.
    top: bool,
}

/// Why a click must not go ahead: the element under the point is not the one the model was
/// shown (and told about) before the pointer travelled there. A page can move or replace what
/// is under the point as the pointer arrives (a hover menu, a banner sliding in, a re-render),
/// and pressing then would click something nobody chose. `None`: the same spot, press on.
fn under_point_changed(before: &Hit, now: &Hit) -> Option<String> {
    if before.node == now.node {
        return None;
    }
    Some(format!(
        "the element under the point changed while the pointer moved: before it was {}, now it is {}; nothing was pressed. Take a new screenshot and pick the point again",
        before.what, now.what
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_viewports_are_not_scaled_and_wide_ones_fit_the_edge() {
        assert!((pointer_scale(800.0) - 1.0).abs() < f64::EPSILON);
        assert!((pointer_scale(1280.0) - 1.0).abs() < f64::EPSILON);
        let s = pointer_scale(2560.0);
        assert!((s - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_screenshot_point_maps_back_to_css_and_off_picture_points_are_refused() {
        let v = PointerView {
            css_width: 2560.0,
            css_height: 1440.0,
            page_x: 0.0,
            page_y: 0.0,
        };
        assert_eq!(v.image_size(), (1280, 720));
        let (x, y) = v.to_css((640.0, 360.0)).unwrap();
        assert!((x - 1280.0).abs() < 1e-9 && (y - 720.0).abs() < 1e-9);
        for bad in [(-1.0, 5.0), (1280.0, 5.0), (5.0, 720.0), (f64::NAN, 1.0)] {
            let e = v.to_css(bad).unwrap_err().to_string();
            assert!(e.contains("outside the screenshot"), "{e}");
        }
    }

    fn seen(node: Option<(i64, &str)>, what: &str) -> Hit {
        Hit {
            node: node.map(|(id, f)| (id, f.to_owned())),
            what: what.to_owned(),
            origin: None,
            top: node.is_none(),
        }
    }

    #[test]
    fn the_same_node_under_the_point_lets_the_press_go_on() {
        let a = seen(Some((7, "F1")), "<button> \"Pay\"");
        let b = seen(Some((7, "F1")), "<button> \"Pay\"");
        assert_eq!(under_point_changed(&a, &b), None);
        let bg = seen(None, "nothing but the page background");
        assert_eq!(under_point_changed(&bg, &seen(None, "background")), None);
    }

    #[test]
    fn another_node_a_new_frame_or_a_vanished_one_is_reported_with_both_names() {
        let before = seen(Some((7, "F1")), "<button> \"Pay\"");
        for now in [
            seen(Some((9, "F1")), "<div role=dialog> \"Cookies\""),
            seen(Some((7, "F2")), "<button> \"Pay\""),
            seen(None, "nothing but the page background"),
        ] {
            let why = under_point_changed(&before, &now).expect("a change");
            assert!(why.contains("<button> \"Pay\""), "{why}");
            assert!(why.contains(&now.what), "{why}");
            assert!(why.contains("nothing was pressed"), "{why}");
            assert!(why.contains("new screenshot"), "{why}");
        }
        // Something laid over an empty spot counts too.
        let bg = seen(None, "nothing but the page background");
        assert!(under_point_changed(&bg, &seen(Some((3, "F1")), "<div>")).is_some());
    }

    #[test]
    fn describing_a_grounded_shot_lists_frames_with_their_centres() {
        let g = Grounded {
            bytes: Vec::new(),
            image: (1000, 600),
            view: PointerView {
                css_width: 1000.0,
                css_height: 600.0,
                page_x: 0.0,
                page_y: 0.0,
            },
            regions: vec![Region {
                kind: "iframe".into(),
                label: "challenges.example".into(),
                x: 100,
                y: 200,
                w: 300,
                h: 65,
            }],
        };
        let d = g.describe();
        assert!(d.contains("image 1000x600 px"), "{d}");
        assert!(
            d.contains("iframe \"challenges.example\" at x=100 y=200 w=300 h=65 (centre 250,232)"),
            "{d}"
        );
    }
}
