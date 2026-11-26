//! `browser_pointer`: act by coordinates read off a screenshot.
//!
//! Refs (`browser_click`, `browser_fill`) are the first choice: they are exact and checked.
//! Some things have no ref: a checkbox inside another site's frame (an "are you human"
//! widget), a canvas, a control drawn with plain `<div>`s. For those the model takes a
//! screenshot (`browser_screenshot`), picks a point and acts there with the same control
//! gate, approval, CAPTCHA budget and secret-field refusals as every other browser action.
//! Every pointer result carries a fresh screenshot, so the next point is read off the page
//! as it is now. No CAPTCHA is solved here: an image or puzzle challenge is a hand-over to the
//! owner (`browser_handover`), never a loop of guesses.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, PermissionCategory, ToolError, ToolSpec};

use super::{BrowserDriver, BrowserTool, PointerAct, PointerOpts, STEP_GUARD, step_guard};
use crate::util::{exec_err, parse, save_file, spec};

/// Longest text one `type` action sends.
const MAX_TYPE_CHARS: usize = 2000;
/// Largest wheel step, in pixels.
const MAX_WHEEL: f64 = 5000.0;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PointerIn {
    action: String,
    x: Option<f64>,
    y: Option<f64>,
    to_x: Option<f64>,
    to_y: Option<f64>,
    dx: Option<f64>,
    dy: Option<f64>,
    text: Option<String>,
    key: Option<String>,
    dialog: Option<String>,
}

impl PointerIn {
    /// The request, or why it makes no sense (said plainly, so the model can fix it).
    pub(super) fn opts(self) -> Result<PointerOpts, ToolError> {
        let bad = |m: &str| Err(ToolError::InvalidInput(m.into()));
        let point = |x: Option<f64>, y: Option<f64>, what: &str| -> Result<(f64, f64), ToolError> {
            match (x, y) {
                (Some(x), Some(y)) if x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 => {
                    Ok((x, y))
                }
                _ => Err(ToolError::InvalidInput(format!(
                    "{what} needs plain non-negative numbers, in pixels of the last browser_screenshot"
                ))),
            }
        };
        let (act, at) = match self.action.as_str() {
            a @ ("click" | "double_click" | "right_click") => (
                PointerAct::Click {
                    right: a == "right_click",
                    double: a == "double_click",
                },
                Some(point(self.x, self.y, "x and y")?),
            ),
            "move" => (PointerAct::Move, Some(point(self.x, self.y, "x and y")?)),
            "drag" => (
                PointerAct::Drag {
                    to: point(self.to_x, self.to_y, "to_x and to_y")?,
                },
                Some(point(self.x, self.y, "x and y (where the drag starts)")?),
            ),
            "scroll" => {
                let (dx, dy) = (self.dx.unwrap_or(0.0), self.dy.unwrap_or(0.0));
                if !dx.is_finite() || !dy.is_finite() || (dx == 0.0 && dy == 0.0) {
                    return bad("scroll needs dy (pixels, positive = down) and/or dx");
                }
                if dx.abs() > MAX_WHEEL || dy.abs() > MAX_WHEEL {
                    return bad("scroll at most 5000 pixels per call");
                }
                (
                    PointerAct::Scroll { dx, dy },
                    Some(point(self.x, self.y, "x and y (where the wheel turns)")?),
                )
            }
            "type" => {
                let text = self.text.unwrap_or_default();
                if text.is_empty() {
                    return bad("type needs `text`");
                }
                if text.chars().count() > MAX_TYPE_CHARS {
                    return bad("type at most 2000 characters per call");
                }
                (PointerAct::Type(text), None)
            }
            "key" => match self.key.filter(|k| !k.trim().is_empty()) {
                Some(k) => (PointerAct::Key(k), None),
                None => return bad("key needs `key`, for example Enter or Control+a"),
            },
            _ => {
                return bad(
                    "action must be click, double_click, right_click, move, drag, scroll, type or key",
                );
            }
        };
        if let Some(d) = &self.dialog
            && !matches!(d.as_str(), "accept" | "dismiss")
        {
            return bad("dialog must be accept or dismiss");
        }
        Ok(PointerOpts {
            act,
            at,
            dialog: self.dialog,
        })
    }
}

/// The permission action and label the control gate is asked about.
fn gate_action(o: &PointerOpts) -> (&'static str, String) {
    let at =
        o.at.map(|(x, y)| format!("({x:.0},{y:.0})"))
            .unwrap_or_default();
    match &o.act {
        PointerAct::Click { .. } | PointerAct::Drag { .. } => ("click", at),
        PointerAct::Move => ("hover", at),
        PointerAct::Scroll { .. } => ("scroll", at),
        PointerAct::Type(_) => ("type", "focused field".to_owned()),
        PointerAct::Key(k) => ("key", k.clone()),
    }
}

pub(super) fn tool_spec(category: PermissionCategory) -> ToolSpec {
    spec(
        "browser_pointer",
        "Act by COORDINATES when browser_click has no ref for the control: a checkbox or button inside another site's frame (an \"I'm not a robot\" box), a canvas, a control drawn without a role. First browser_screenshot, pick the middle of the control in that picture's pixels, then call this; the result names what is under the point and ends with a fresh screenshot to check. action: click, double_click, right_click, move, drag (x,y to to_x,to_y), scroll (dy pixels, down positive), type (into the focused field: click it first; never passwords, codes or cards), key. Prefer refs when there is one. One \"not a robot\" checkbox may be clicked; an image, tile, slider or press-and-hold puzzle is NOT solved: browser_handover, else report it blocked and move on. Example: {\"action\":\"click\",\"x\":412,\"y\":238}",
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["click","double_click","right_click","move","drag","scroll","type","key"]},
            "x":{"type":"number","minimum":0,"description":"Pixels from the left of the screenshot."},
            "y":{"type":"number","minimum":0,"description":"Pixels from the top of the screenshot."},
            "to_x":{"type":"number","minimum":0,"description":"drag: where it is released."},
            "to_y":{"type":"number","minimum":0},
            "dx":{"type":"number","description":"scroll: pixels right."},
            "dy":{"type":"number","description":"scroll: pixels down."},
            "text":{"type":"string","description":"type: text for the focused field."},
            "key":{"type":"string","description":"key: Enter, Tab, Escape, Control+a ..."},
            "dialog":{"type":"string","enum":["accept","dismiss"]}
        },"required":["action"]}),
        Effect::Write,
        category,
    )
}

impl BrowserTool {
    /// `browser_pointer`: gate, act, and return the refreshed view with a fresh screenshot.
    pub(super) async fn run_pointer(
        &self,
        run_id: &str,
        input: Value,
    ) -> Result<String, ToolError> {
        let opts = parse::<PointerIn>(input)?.opts()?;
        let d = Arc::clone(&self.shared.driver);
        let (action, label) = gate_action(&opts);
        let permit = self.gate(run_id, action, label).await?;
        if let Some(fresh) = self.discard_if_stale(&permit, &d).await? {
            return Ok(fresh);
        }
        let guard = step_guard();
        let acted = {
            let d = Arc::clone(&d);
            permit
                .protect(STEP_GUARD.scope(guard, async move { d.pointer(&opts).await }))
                .await
                .map_err(exec_err)?
        };
        let mut text = acted.map_err(exec_err)?;
        text.push_str(&self.fresh_shot(&d).await);
        Ok(text)
    }

    /// A screenshot made for pointing at, saved under the data directory; the text lines that
    /// tell the model where it is and how to use it. Empty when the browser cannot take one
    /// right now (a typed one-time code is on screen, the driver cannot, ...): the action
    /// result stands without it.
    pub(super) async fn fresh_shot(&self, d: &Arc<dyn BrowserDriver>) -> String {
        // The same rule as `browser_screenshot`: no picture while a typed one-time code is
        // visible on the page.
        if crate::handover::handover_hub().capture_latched(d).await {
            return String::new();
        }
        match self.save_grounded(d).await {
            Ok(text) => format!("\n{text}"),
            Err(_) => String::new(),
        }
    }

    /// `screenshot: <path>` plus the pointer notes.
    pub(super) async fn save_grounded(
        &self,
        d: &Arc<dyn BrowserDriver>,
    ) -> Result<String, ToolError> {
        let shot = d.grounded_screenshot().await.map_err(exec_err)?;
        let path = save_file(
            &self.shared.data_dir.join("screenshots"),
            shot.ext,
            &shot.bytes,
        )?;
        if let Some(run) = super::current_run() {
            super::sideband::note_screenshot(&run, std::path::Path::new(&path));
        }
        let mut out = format!("screenshot: {path} ({}x{})", shot.width, shot.height);
        let _ = write!(out, "\n{}", shot.notes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(v: Value) -> Result<PointerOpts, ToolError> {
        parse::<PointerIn>(v)?.opts()
    }

    #[test]
    fn click_variants_need_a_point_inside_the_picture_space() {
        let o = parsed(json!({"action":"right_click","x":10,"y":20.5})).unwrap();
        assert_eq!(
            o.act,
            PointerAct::Click {
                right: true,
                double: false
            }
        );
        assert_eq!(o.at, Some((10.0, 20.5)));
        let o = parsed(json!({"action":"double_click","x":1,"y":2})).unwrap();
        assert_eq!(
            o.act,
            PointerAct::Click {
                right: false,
                double: true
            }
        );
        for bad in [
            json!({"action":"click"}),
            json!({"action":"click","x":5}),
            json!({"action":"click","x":-1,"y":5}),
        ] {
            assert!(parsed(bad).is_err());
        }
    }

    #[test]
    fn drag_scroll_type_and_key_are_validated() {
        assert!(parsed(json!({"action":"drag","x":1,"y":2})).is_err());
        let o = parsed(json!({"action":"drag","x":1,"y":2,"to_x":30,"to_y":40})).unwrap();
        assert_eq!(o.act, PointerAct::Drag { to: (30.0, 40.0) });
        assert!(parsed(json!({"action":"scroll","x":1,"y":2})).is_err());
        assert!(parsed(json!({"action":"scroll","x":1,"y":2,"dy":99999})).is_err());
        let o = parsed(json!({"action":"scroll","x":1,"y":2,"dy":-300})).unwrap();
        assert_eq!(
            o.act,
            PointerAct::Scroll {
                dx: 0.0,
                dy: -300.0
            }
        );
        assert!(parsed(json!({"action":"type"})).is_err());
        assert!(parsed(json!({"action":"type","text":"x".repeat(2001)})).is_err());
        let o = parsed(json!({"action":"type","text":"hi"})).unwrap();
        assert_eq!((o.act, o.at), (PointerAct::Type("hi".into()), None));
        assert!(parsed(json!({"action":"key","key":" "})).is_err());
        let o = parsed(json!({"action":"key","key":"Enter"})).unwrap();
        assert_eq!(o.act, PointerAct::Key("Enter".into()));
    }

    #[test]
    fn unknown_actions_dialogs_and_a_secret_parameter_are_refused() {
        assert!(parsed(json!({"action":"teleport","x":1,"y":2})).is_err());
        assert!(parsed(json!({"action":"click","x":1,"y":2,"dialog":"maybe"})).is_err());
        // Typing a stored secret is browser_fill's job, with its own card on every call.
        let e = parsed(json!({"action":"type","text":"x","secret":"login/github"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("secret"), "{e}");
    }

    #[test]
    fn the_gate_is_asked_the_right_question() {
        let click = parsed(json!({"action":"click","x":412.4,"y":238.0})).unwrap();
        assert_eq!(gate_action(&click), ("click", "(412,238)".to_owned()));
        let mv = parsed(json!({"action":"move","x":1,"y":2})).unwrap();
        assert_eq!(gate_action(&mv).0, "hover");
        let key = parsed(json!({"action":"key","key":"Enter"})).unwrap();
        assert_eq!(gate_action(&key), ("key", "Enter".to_owned()));
        let typed = parsed(json!({"action":"type","text":"x"})).unwrap();
        assert_eq!(gate_action(&typed).0, "type");
    }
}
