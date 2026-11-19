//! Auto-waiting before an action: the element must be attached, visible, enabled, not moving and
//! (for clicks) the thing that would actually receive the pointer event.
//!
//! One in-page probe per poll. The decision ([`judge`]) is a pure function of the probe result so
//! it is unit tested without a browser.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::page::Page;
use crate::snapshot::RefEntry;

/// How long an action waits for its element to become actionable.
pub const ACTION_WAIT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(80);

/// What the action needs from the element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    /// Pointer target: must be hit by a click at its centre.
    Click,
    /// Text entry: must be an editable, writable field.
    Edit,
    /// Only attached, visible and enabled (hover, select, key-by-key typing).
    Present,
}

/// Probe run with the element as `this`. Arguments: `(wantHit, wantEdit, recenter)`.
const PROBE_JS: &str = r"function(wantHit, wantEdit, recenter) {
  const el = this.nodeType === 1 ? this : this.parentElement;
  if (!el || !el.isConnected) return {state: 'detached'};
  if (recenter) { try { el.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'}); } catch (e) {} }
  const tag = el.tagName.toLowerCase();
  const type = (el.getAttribute('type') || '').toLowerCase();
  let kind = 'other';
  if (tag === 'input') kind = 'input';
  else if (tag === 'textarea') kind = 'textarea';
  else if (tag === 'select') kind = 'select';
  else if (el.isContentEditable) kind = 'contenteditable';
  let r = null;
  for (const c of el.getClientRects()) { if (c.width >= 1 && c.height >= 1) { r = c; break; } }
  const cs = getComputedStyle(el);
  if (!r || cs.visibility === 'hidden' || cs.visibility === 'collapse' || cs.display === 'none')
    return {state: 'hidden', tag, kind, type};
  const box = [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)];
  let disabled = el.matches(':disabled');
  for (let n = el; n && !disabled; n = n.parentElement) {
    if (n.getAttribute('aria-disabled') === 'true') disabled = true;
  }
  if (disabled) return {state: 'disabled', tag, kind, type, box};
  if (wantEdit) {
    const notText = ['checkbox', 'radio', 'button', 'submit', 'reset', 'file', 'image', 'hidden'];
    if (kind === 'other' || kind === 'select' || (kind === 'input' && notText.includes(type)))
      return {state: 'not_editable', tag, kind, type, box};
    if (el.readOnly) return {state: 'readonly', tag, kind, type, box};
  }
  if (wantHit) {
    const x = r.left + r.width / 2, y = r.top + r.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight)
      return {state: 'offscreen', tag, kind, type, box};
    let hit = document.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    let ok = false;
    for (let n = hit; n; n = n.parentNode || n.host) { if (n === el) { ok = true; break; } }
    if (!ok && hit && hit.closest) {
      const lab = hit.closest('label');
      if (lab && (lab.control === el || lab.contains(el))) ok = true;
      if (!ok && el.labels) { for (const l of el.labels) { if (l.contains(hit)) ok = true; } }
    }
    if (!ok) {
      let by = 'nothing';
      if (hit) {
        const clean = (t) => (t || '').trim().replace(/\s+/g, ' ').slice(0, 60);
        const dlg = hit.closest && hit.closest('dialog,[role=dialog],[role=alertdialog],[aria-modal=true]');
        if (dlg) {
          let nm = clean(dlg.getAttribute('aria-label'));
          if (!nm) {
            const ids = (dlg.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
            nm = clean(ids.map((i) => { const n = document.getElementById(i); return n ? n.innerText : ''; }).join(' '));
          }
          if (!nm) { const h = dlg.querySelector('h1,h2,h3,[role=heading]'); if (h) nm = clean(h.innerText); }
          by = 'dialog' + (nm ? ' «' + nm + '»' : '');
        } else {
          by = '<' + hit.tagName.toLowerCase();
          if (hit.id) by += '#' + hit.id;
          const cls = (hit.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean).slice(0, 2);
          if (cls.length) by += '.' + cls.join('.');
          by += '>';
          const t = clean(hit.innerText || hit.getAttribute('aria-label'));
          if (t) by += ' ' + JSON.stringify(t);
        }
      }
      return {state: 'covered', tag, kind, type, box, by};
    }
  }
  return {state: 'ok', tag, kind, type, box};
}";

/// One probe result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Probe {
    pub state: String,
    pub tag: String,
    /// `input`, `textarea`, `select`, `contenteditable` or `other`.
    pub kind: String,
    /// The `type` attribute of an input, lower-cased.
    pub input_type: String,
    /// Client rectangle `[x, y, w, h]` in the element's own frame.
    pub bbox: Option<[i64; 4]>,
    /// Description of the element that would receive the click instead.
    pub by: String,
}

impl Probe {
    pub(crate) fn parse(v: &Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let bbox = v["box"].as_array().and_then(|a| {
            let n: Vec<i64> = a.iter().filter_map(Value::as_i64).collect();
            <[i64; 4]>::try_from(n).ok()
        });
        Self {
            state: s("state"),
            tag: s("tag"),
            kind: s("kind"),
            input_type: s("type"),
            bbox,
            by: sanitize(&s("by")),
        }
    }
}

/// Page text that ends up in an error message: one line, no control characters, bounded.
fn sanitize(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.chars().take(120).collect()
}

/// Verdict for one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Act now.
    Ready,
    /// Not yet; the reason is reported if the wait runs out.
    Wait(String),
    /// Waiting cannot help.
    Fail(String),
}

/// Decide from two consecutive probes (`prev` is the previous poll, if any).
pub(crate) fn judge(what: &str, prev: Option<&Probe>, cur: &Probe) -> Verdict {
    match cur.state.as_str() {
        "ok" => {
            if prev.is_some_and(|p| p.state == "ok" && p.bbox == cur.bbox) {
                Verdict::Ready
            } else {
                Verdict::Wait(format!("{what} keeps moving (animation or layout shift)"))
            }
        }
        "detached" => Verdict::Fail(format!(
            "{what} is no longer in the page; take a new browser_snapshot and use its refs"
        )),
        "not_editable" => {
            let hint = match (cur.kind.as_str(), cur.input_type.as_str()) {
                ("select", _) => "use browser_select to choose an option",
                (_, "checkbox" | "radio") => "use browser_click to toggle it",
                (_, "file") => "file inputs cannot be typed into",
                _ => "use browser_click, or pick the text field inside it",
            };
            Verdict::Fail(format!(
                "{what} is a <{}> and not a text field; {hint}",
                cur.tag
            ))
        }
        "readonly" => Verdict::Fail(format!("{what} is read-only and cannot be filled")),
        "hidden" => Verdict::Wait(format!(
            "{what} is not visible (hidden or zero size); open the menu, tab or section that shows it first"
        )),
        "disabled" => Verdict::Wait(format!(
            "{what} is disabled; complete the required fields or wait until it is enabled"
        )),
        "offscreen" => Verdict::Wait(format!("{what} could not be scrolled into view")),
        "covered" => Verdict::Wait(format!(
            "{what} is covered by {} which would receive the click instead; close or dismiss it first, then retry",
            cur.by
        )),
        other => Verdict::Wait(format!("{what} is not ready ({other})")),
    }
}

impl Page {
    async fn probe(&self, e: &RefEntry, need: Need, recenter: bool) -> Result<Probe> {
        // The hit test runs in the element's own frame at the centre of its first client rect,
        // which is the border-box centre `Page::center` clicks (it reads the border quad).
        let v = self
            .call_on(
                e,
                PROBE_JS,
                &[
                    json!(need == Need::Click),
                    json!(need == Need::Edit),
                    json!(recenter),
                ],
            )
            .await;
        match v {
            Ok(v) => Ok(Probe::parse(&v)),
            // The node behind the ref is gone (resolveNode failed).
            Err(Error::Element(_)) => Ok(Probe {
                state: "detached".into(),
                ..Probe::default()
            }),
            Err(e) => Err(e),
        }
    }

    /// Scroll the element into view and wait until it can take the action. Returns the last
    /// probe, or an [`Error::Element`] that says what is wrong and what to do about it.
    pub(crate) async fn wait_actionable(
        &self,
        r: &str,
        e: &RefEntry,
        need: Need,
        timeout: Duration,
    ) -> Result<Probe> {
        let what = describe(r, e);
        let _ = self
            .cmd_on(
                e.session_id.as_deref(),
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": e.backend_id}),
            )
            .await;
        let deadline = Instant::now() + timeout;
        let mut prev: Option<Probe> = None;
        let mut recentered = false;
        loop {
            // A sticky header or footer can cover an element that is technically in view:
            // centre it once before giving up on a covered or off-screen target.
            let recenter = !recentered
                && prev
                    .as_ref()
                    .is_some_and(|p| p.state == "covered" || p.state == "offscreen");
            recentered |= recenter;
            let cur = self.probe(e, need, recenter).await?;
            match judge(&what, prev.as_ref(), &cur) {
                Verdict::Ready => return Ok(cur),
                Verdict::Fail(m) => return Err(Error::Element(m)),
                Verdict::Wait(m) => {
                    if Instant::now() >= deadline {
                        return Err(Error::Element(format!(
                            "{m} (waited {:.1}s)",
                            timeout.as_secs_f64()
                        )));
                    }
                }
            }
            prev = Some(cur);
            tokio::time::sleep(POLL).await;
        }
    }
}

/// `e12 (button "Save")` for error messages.
pub(crate) fn describe(r: &str, e: &RefEntry) -> String {
    let r = crate::snapshot::RefMap::normalize(r);
    if e.name.trim().is_empty() {
        format!("{r} ({})", e.role)
    } else {
        let name: String = sanitize(&e.name).chars().take(60).collect();
        format!("{r} ({} \"{}\")", e.role, name.replace('"', "'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(state: &str, bbox: [i64; 4]) -> Probe {
        Probe {
            state: state.into(),
            tag: "button".into(),
            kind: "other".into(),
            bbox: Some(bbox),
            ..Probe::default()
        }
    }

    #[test]
    fn ready_needs_two_identical_ok_samples() {
        let a = probe("ok", [10, 10, 80, 20]);
        assert!(matches!(judge("e1", None, &a), Verdict::Wait(_)));
        assert_eq!(judge("e1", Some(&a), &a), Verdict::Ready);
        let moved = probe("ok", [10, 30, 80, 20]);
        let Verdict::Wait(m) = judge("e1", Some(&a), &moved) else {
            panic!("a moving element must wait")
        };
        assert!(m.contains("moving"), "{m}");
    }

    #[test]
    fn covered_names_the_obstacle_and_the_way_out() {
        let mut p = probe("covered", [0, 0, 10, 10]);
        p.by = "<div#cookies.banner> \"Accept all\"".into();
        let Verdict::Wait(m) = judge("e7 (button \"Buy\")", None, &p) else {
            panic!()
        };
        assert!(m.contains("e7 (button \"Buy\")") && m.contains("<div#cookies.banner>"));
        assert!(m.contains("dismiss"), "{m}");
    }

    #[test]
    fn detached_and_wrong_kind_fail_without_waiting() {
        let Verdict::Fail(m) = judge("e3", None, &probe("detached", [0; 4])) else {
            panic!()
        };
        assert!(m.contains("browser_snapshot"), "{m}");
        let mut sel = probe("not_editable", [0; 4]);
        sel.kind = "select".into();
        sel.tag = "select".into();
        let Verdict::Fail(m) = judge("e4", None, &sel) else {
            panic!()
        };
        assert!(m.contains("browser_select"), "{m}");
        let mut cb = probe("not_editable", [0; 4]);
        cb.kind = "input".into();
        cb.tag = "input".into();
        cb.input_type = "checkbox".into();
        let Verdict::Fail(m) = judge("e5", None, &cb) else {
            panic!()
        };
        assert!(m.contains("browser_click"), "{m}");
    }

    #[test]
    fn hidden_and_disabled_wait_with_a_hint() {
        for (state, word) in [("hidden", "not visible"), ("disabled", "disabled")] {
            let Verdict::Wait(m) = judge("e1", None, &probe(state, [0; 4])) else {
                panic!("{state}")
            };
            assert!(m.contains(word), "{m}");
        }
    }

    #[test]
    fn probe_parse_sanitizes_page_text() {
        let p = Probe::parse(&json!({
            "state": "covered", "tag": "a", "kind": "other", "type": "",
            "box": [1, 2, 3, 4], "by": format!("<div> \"x\ny{}\"", "z".repeat(500)),
        }));
        assert_eq!(p.bbox, Some([1, 2, 3, 4]));
        assert!(!p.by.contains('\n'));
        assert!(p.by.chars().count() <= 120);
    }

    #[test]
    fn describe_is_compact() {
        let e = RefEntry {
            session_id: None,
            backend_id: 1,
            frame_id: "f".into(),
            role: "button".into(),
            name: "Save \"now\"".into(),
        };
        assert_eq!(describe("[e12]", &e), "e12 (button \"Save 'now'\")");
    }
}
