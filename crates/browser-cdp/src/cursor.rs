//! The bot's own pointer, drawn inside the page.
//!
//! Computer-use apps show a pointer of their own, distinct from the owner's, so a person
//! watching can see where the bot is about to click. CDP input never moves the real OS cursor,
//! so for the managed browser the pointer is a small overlay in the page itself:
//!
//! * a `<div>` host on the document element, `position: fixed`, `pointer-events: none`, the
//!   highest `z-index`, `aria-hidden`, holding the arrow and the click ripples inside a
//!   **closed shadow root**. It can never be hit by a click (`Page::hit_at` uses
//!   `DOM.getNodeForLocation` with `ignorePointerEventsNone: false`), is not in the
//!   accessibility tree, has no text, and is not reached by `querySelectorAll`;
//! * the script runs in an **isolated world** (`Page.createIsolatedWorld`), so page scripts
//!   cannot call into it or read its state. It is told coordinates and nothing else: no page
//!   text, no field value, nothing secret ever goes in or comes out;
//! * nothing is persistent: no `addScriptToEvaluateOnNewDocument`. The host is created lazily
//!   by the next pointer action after a navigation, fades out by itself after
//!   [`IDLE_FADE_MS`] without one, and [`Page::bot_cursor_hide`] removes it completely (hand
//!   over to the owner, owner takes control in the live view).
//!
//! Drawing is best effort: short timeout, errors ignored, never fails the action, and a no-op
//! while the owner's switch (`live_view::bot_cursor::enabled`) is off.
//!
//! The live view needs nothing from here: its screencast shows the page, overlay included.
//! Browser positions are therefore **not** published to `live_view::bot_cursor` (that is for
//! the desktop, whose frames are plain pixels), which would draw the pointer twice.
//! The grounded screenshot a model reads keeps the overlay in: it shows where the pointer
//! last was, and the arrow tip covers a pixel or two at most.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};

use crate::client::CdpClient;
use crate::page::{Page, Shared};

/// Name of the isolated world the overlay script lives in.
pub(crate) const WORLD_NAME: &str = "praxi-bot-cursor";

/// The overlay fades out after this long without a pointer action, so a bot that finished
/// does not leave a pointer parked on the page.
pub const IDLE_FADE_MS: u32 = 10_000;

/// Longest a drawing call may take before it is given up on.
const DRAW_TIMEOUT: Duration = Duration::from_millis(500);

/// One drawing command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorOp {
    /// Glide to `to` (CSS viewport pixels) in `ms`, starting from `from` when the overlay
    /// has no position of its own yet.
    Move {
        /// Where the pointer was before, if known.
        from: Option<(f64, f64)>,
        /// Where it goes.
        to: (f64, f64),
        /// Animation length.
        ms: u32,
    },
    /// A click ripple at `at`.
    Click {
        /// Where.
        at: (f64, f64),
    },
    /// Remove the overlay completely.
    Hide,
}

/// How long the overlay takes to glide `distance` CSS pixels: quick for short hops, a little
/// over half a second for a trip across a wide screen.
#[must_use]
pub fn move_duration_ms(distance: f64) -> u32 {
    if !distance.is_finite() || distance < 1.0 {
        return 0;
    }
    (120.0 + distance * 0.35).clamp(120.0, 650.0).round() as u32
}

/// Whether two successive readings of an element's centre are the same spot: a smooth scroll
/// has finished when the element stopped moving.
#[must_use]
pub fn centres_agree(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 0.75 && (a.1 - b.1).abs() < 0.75
}

/// Whether the element has stopped moving: at least two readings, the last two agreeing.
#[must_use]
pub fn scroll_settled(readings: &[(f64, f64)]) -> bool {
    match readings {
        [.., a, b] => centres_agree(*a, *b),
        _ => false,
    }
}

/// The overlay script: a function of `(op, fx, fy, x, y, ms, fadeMs)`. It builds the host on
/// first use (again after a navigation), animates with the Web Animations API and sets styles
/// through the CSSOM only, so a strict page CSP or Trusted Types policy cannot stop it.
const SCRIPT: &str = r"(op, fx, fy, x, y, ms, fadeMs) => {
  const g = globalThis;
  let S = g.__praxiBotCursor;
  if (!S || S.doc !== document) { S = g.__praxiBotCursor = { doc: document }; }
  const hide = () => {
    if (S.timer) { clearTimeout(S.timer); S.timer = 0; }
    for (const n of document.querySelectorAll('[data-praxi-bot-cursor]')) n.remove();
    S.host = S.cur = S.root = null; S.x = S.y = undefined;
  };
  if (op === 'hide') { hide(); return 'hidden'; }
  const root0 = document.documentElement;
  if (!root0) { return 'nodoc'; }
  const T = (px, py) => 'translate(' + px + 'px,' + py + 'px)';
  let fresh = false;
  if (!S.host || !S.host.isConnected) {
    hide();
    const host = document.createElement('div');
    host.setAttribute('data-praxi-bot-cursor', '');
    host.setAttribute('aria-hidden', 'true');
    Object.assign(host.style, { all: 'initial', display: 'block', position: 'fixed', left: '0',
      top: '0', width: '0', height: '0', overflow: 'visible', pointerEvents: 'none',
      zIndex: '2147483647' });
    const root = host.attachShadow({ mode: 'closed' });
    const NS = 'http://www.w3.org/2000/svg';
    const cur = document.createElement('div');
    Object.assign(cur.style, { position: 'absolute', left: '0', top: '0', pointerEvents: 'none',
      willChange: 'transform', filter: 'drop-shadow(0 1px 2px rgba(0,0,0,0.45))' });
    const svg = document.createElementNS(NS, 'svg');
    svg.setAttribute('width', '26'); svg.setAttribute('height', '26');
    svg.setAttribute('viewBox', '0 0 26 26');
    Object.assign(svg.style, { display: 'block', marginLeft: '-3px', marginTop: '-2px' });
    const path = document.createElementNS(NS, 'path');
    path.setAttribute('d', 'M3 2 L3 20 L7.6 15.8 L10.8 23 L14.2 21.6 L11 14.6 L17.6 14.4 Z');
    path.setAttribute('fill', '#7c3aed'); path.setAttribute('stroke', '#ffffff');
    path.setAttribute('stroke-width', '1.6'); path.setAttribute('stroke-linejoin', 'round');
    svg.appendChild(path); cur.appendChild(svg); root.appendChild(cur);
    root0.appendChild(host);
    S.host = host; S.cur = cur; S.root = root; fresh = true;
  }
  const cur = S.cur;
  cur.style.opacity = '1';
  const place = (ax, ay, bx, by, dur) => {
    cur.style.transform = T(bx, by);
    if (dur > 0 && (ax !== bx || ay !== by)) {
      cur.animate([{ transform: T(ax, ay) }, { transform: T(bx, by) }],
        { duration: dur, easing: 'cubic-bezier(0.22,0.8,0.26,1)' });
    }
  };
  if (op === 'move' || (op === 'click' && S.x === undefined)) {
    const ax = S.x !== undefined ? S.x : (fx !== null ? fx : x);
    const ay = S.y !== undefined ? S.y : (fy !== null ? fy : y);
    place(ax, ay, x, y, op === 'move' ? ms : 0);
    if (fresh) { cur.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 160 }); }
    S.x = x; S.y = y;
  }
  if (op === 'click') {
    const ring = document.createElement('div');
    Object.assign(ring.style, { position: 'absolute', left: x + 'px', top: y + 'px', width: '0',
      height: '0', pointerEvents: 'none' });
    const dot = document.createElement('div');
    Object.assign(dot.style, { position: 'absolute', left: '-20px', top: '-20px', width: '40px',
      height: '40px', boxSizing: 'border-box', borderRadius: '50%',
      border: '3px solid #7c3aed', background: 'rgba(124,58,237,0.18)', pointerEvents: 'none' });
    ring.appendChild(dot); S.root.appendChild(ring);
    const a = dot.animate([{ transform: 'scale(0.25)', opacity: 0.95 },
      { transform: 'scale(1.35)', opacity: 0 }], { duration: 520, easing: 'ease-out' });
    a.onfinish = () => ring.remove();
    cur.animate([{ transform: T(x, y) + ' scale(1)' }, { transform: T(x, y) + ' scale(0.82)' },
      { transform: T(x, y) + ' scale(1)' }], { duration: 220 });
  }
  if (S.timer) { clearTimeout(S.timer); }
  S.timer = setTimeout(() => {
    if (!S.cur) { return; }
    const a = S.cur.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 600, fill: 'forwards' });
    a.onfinish = hide;
  }, fadeMs);
  return 'ok';
}";

/// The expression that runs the injected cursor script for `op`. Numbers only: nothing from the page or the
/// owner goes into it.
#[must_use]
pub fn expression(op: &CursorOp) -> String {
    let num = |v: f64| if v.is_finite() { json!(v) } else { json!(0) };
    let (name, from, at, ms) = match *op {
        CursorOp::Move { from, to, ms } => ("move", from, to, ms),
        CursorOp::Click { at } => ("click", None, at, 0),
        CursorOp::Hide => ("hide", None, (0.0, 0.0), 0),
    };
    let (fx, fy) = from.map_or((Value::Null, Value::Null), |(a, b)| (num(a), num(b)));
    format!(
        "({SCRIPT})({name:?},{fx},{fy},{},{},{ms},{IDLE_FADE_MS})",
        num(at.0),
        num(at.1)
    )
}

/// Run one command in the overlay's isolated world; `Err(())` when it could not be drawn.
async fn run(client: &CdpClient, session: &str, shared: &Shared, op: &CursorOp) -> Result<(), ()> {
    let expression = expression(op);
    for attempt in 0..2 {
        let nav = shared.nav_gen.load(Ordering::SeqCst);
        let cached = shared
            .cursor_ctx
            .lock()
            .ok()
            .and_then(|c| *c)
            .filter(|(n, _)| *n == nav);
        let ctx = if let Some((_, ctx)) = cached {
            ctx
        } else {
            let tree = client
                .send_on(Some(session), "Page.getFrameTree", json!({}))
                .await
                .map_err(|_| ())?;
            let frame = tree["frameTree"]["frame"]["id"].as_str().ok_or(())?;
            let world = client
                .send_on(
                    Some(session),
                    "Page.createIsolatedWorld",
                    json!({"frameId": frame, "worldName": WORLD_NAME}),
                )
                .await
                .map_err(|_| ())?;
            let ctx = world["executionContextId"].as_i64().ok_or(())?;
            if let Ok(mut c) = shared.cursor_ctx.lock() {
                *c = Some((nav, ctx));
            }
            ctx
        };
        let reply = client
            .send_on(
                Some(session),
                "Runtime.evaluate",
                json!({"expression": expression, "contextId": ctx, "returnByValue": true,
                       "silent": true}),
            )
            .await;
        match reply {
            Ok(r) if r.get("exceptionDetails").is_none() => return Ok(()),
            // The document (and its isolated world) went away: make a new world once.
            _ => {
                if let Ok(mut c) = shared.cursor_ctx.lock() {
                    *c = None;
                }
                if attempt == 1 {
                    return Err(());
                }
            }
        }
    }
    Err(())
}

/// Draw `op` best effort, within [`DRAW_TIMEOUT`]. Returns whether it was drawn.
pub(crate) async fn draw(
    client: &CdpClient,
    session: &str,
    shared: &Shared,
    op: &CursorOp,
) -> bool {
    if !matches!(op, CursorOp::Hide) {
        if !live_view::bot_cursor::enabled() {
            return false;
        }
        // Set before the call: a hide racing with this draw must still run.
        shared.cursor_drawn.store(true, Ordering::SeqCst);
    }
    matches!(
        tokio::time::timeout(DRAW_TIMEOUT, run(client, session, shared, op)).await,
        Ok(Ok(()))
    )
}

/// Remove the overlay from the page of `session`, if one may be there. Used by the live-view
/// takeover, which holds the client and session rather than a [`Page`].
pub(crate) async fn hide(client: &CdpClient, session: &str, shared: &Shared) {
    if shared.cursor_drawn.swap(false, Ordering::SeqCst) {
        let _ = draw(client, session, shared, &CursorOp::Hide).await;
    }
}

impl Page {
    /// Glide the bot cursor to `to` (CSS viewport pixels) from where it was.
    pub(crate) async fn bot_cursor_move(&self, to: (f64, f64)) {
        let from = self.shared.pointer.lock().ok().and_then(|p| *p);
        let ms = from.map_or(0, |f| move_duration_ms((to.0 - f.0).hypot(to.1 - f.1)));
        let op = CursorOp::Move { from, to, ms };
        draw(&self.client, &self.session, &self.shared, &op).await;
    }

    /// Show a click ripple at `at` (CSS viewport pixels).
    pub(crate) async fn bot_cursor_click(&self, at: (f64, f64)) {
        draw(
            &self.client,
            &self.session,
            &self.shared,
            &CursorOp::Click { at },
        )
        .await;
    }

    /// Remove the bot cursor from the page completely (the owner takes over, or a hand-over
    /// starts). Costs nothing when none was drawn.
    pub async fn bot_cursor_hide(&self) {
        hide(&self.client, &self.session, &self.shared).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(w: f64, h: f64) -> crate::actions::PointerView {
        crate::actions::PointerView {
            css_width: w,
            css_height: h,
            page_x: 0.0,
            page_y: 0.0,
        }
    }

    #[test]
    fn the_glide_is_quick_for_short_hops_and_bounded_for_long_ones() {
        assert_eq!(move_duration_ms(0.0), 0);
        assert_eq!(move_duration_ms(f64::NAN), 0);
        assert_eq!(move_duration_ms(10.0), 124);
        assert_eq!(move_duration_ms(100_000.0), 650);
        let (a, b) = (move_duration_ms(200.0), move_duration_ms(900.0));
        assert!(a < b && (120..=650).contains(&a) && (120..=650).contains(&b));
    }

    #[test]
    fn the_overlay_is_drawn_in_the_css_pixels_a_screenshot_point_maps_to() {
        // The overlay is `position: fixed` in the top document, in the pixels CDP input uses,
        // so what it is told is exactly `PointerView::to_css` of the point the model named:
        // a 2560 px wide viewport is shown to the model at half size...
        let v = view(2560.0, 1440.0);
        assert_eq!(v.to_css((640.0, 360.0)).unwrap(), (1280.0, 720.0));
        // ...a small one is not scaled at all.
        let v = view(800.0, 600.0);
        assert_eq!(v.to_css((123.0, 45.0)).unwrap(), (123.0, 45.0));
        // A point off the picture never reaches the overlay: the action is refused first.
        assert!(v.to_css((800.0, 10.0)).is_err());
    }

    #[test]
    fn a_scroll_is_settled_only_when_the_last_two_readings_agree() {
        assert!(!scroll_settled(&[]));
        assert!(!scroll_settled(&[(10.0, 10.0)]));
        assert!(!scroll_settled(&[(10.0, 10.0), (10.0, 60.0)]));
        assert!(scroll_settled(&[(10.0, 10.0), (10.0, 60.0), (10.2, 60.4)]));
        assert!(centres_agree((0.0, 0.0), (0.7, -0.7)));
        assert!(!centres_agree((0.0, 0.0), (0.8, 0.0)));
    }

    #[test]
    fn the_expression_carries_numbers_only_and_calls_the_one_script() {
        let e = expression(&CursorOp::Move {
            from: Some((1.5, 2.0)),
            to: (30.0, 40.25),
            ms: 200,
        });
        assert!(
            e.ends_with(r#")("move",1.5,2.0,30.0,40.25,200,10000)"#),
            "{e}"
        );
        assert!(
            expression(&CursorOp::Click { at: (5.0, 6.0) })
                .ends_with(r#")("click",null,null,5.0,6.0,0,10000)"#)
        );
        assert!(expression(&CursorOp::Hide).contains(r#""hide""#));
        // A non-finite point cannot put anything but a number into the script.
        let bad = expression(&CursorOp::Click {
            at: (f64::NAN, f64::INFINITY),
        });
        assert!(bad.ends_with(r#""click",null,null,0,0,0,10000)"#), "{bad}");
    }

    #[test]
    fn the_overlay_is_inert_isolated_and_built_without_markup_strings() {
        // The properties the safety argument rests on, checked on the script text itself.
        for needle in [
            "mode: 'closed'",
            "pointerEvents: 'none'",
            "zIndex: '2147483647'",
            "setAttribute('aria-hidden', 'true')",
            "setAttribute('data-praxi-bot-cursor', '')",
            "n.remove()",
        ] {
            assert!(SCRIPT.contains(needle), "{needle}");
        }
        // Strict CSP / Trusted Types: no markup strings, no inline style attribute, no eval.
        for banned in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "eval(",
            "setAttribute('style'",
            "cssText",
        ] {
            assert!(!SCRIPT.contains(banned), "{banned}");
        }
        // Coordinates only: the script reads nothing from the page.
        for banned in [
            "innerText",
            "textContent",
            "value",
            "cookie",
            "localStorage",
        ] {
            assert!(!SCRIPT.contains(banned), "{banned}");
        }
    }
}
