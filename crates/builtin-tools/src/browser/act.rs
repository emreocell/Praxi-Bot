//! `browser_act`: several browser steps in ONE tool call.
//!
//! A batch is never a shortcut around a single call. Every step runs through the
//! same code as its single tool (`BrowserTool::run_inner`): the same control gate,
//! the same page-level refusals (password / one-time-code / card fields), the same
//! origin check against the page the approval was given for. On top of that every
//! step is verified against a fingerprint (role and accessible name of its ref,
//! the page URL) taken before the first step; a page that changed stops the batch.
//!
//! Not part of a batch, on purpose: `browser_open` (navigation is its own approval
//! with its own URL and profile line), `browser_eval`, tabs and back, the
//! `dialog` / `prompt_text` options of a click, and `secret` fills (a secret is
//! confirmed on every call with its own card, see `browser_fill`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use secret_store::sanitize_label;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{ToolContext, ToolError};

use super::{BrowserTool, Op, expected_input_origin, origin_of};
use crate::util::exec_err;

/// Most steps one call may carry.
pub(super) const MAX_STEPS: usize = 12;
/// Largest batch input (compact JSON). The approval card shows the raw input up to a
/// fixed length; a batch that fits cannot have steps cut off the card.
const MAX_INPUT_CHARS: usize = 2500;
/// Longest a `wait` step may wait.
const MAX_WAIT_SECS: u64 = 15;
/// Total time of one batch; the step running when it ends is cancelled.
const TOTAL_BUDGET: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActIn {
    steps: Vec<Step>,
}

/// One step. Unknown fields (`secret`, `dialog`, `prompt_text`, ...) are refused.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum Step {
    Click {
        r#ref: String,
    },
    Fill {
        r#ref: String,
        text: String,
    },
    Select {
        r#ref: String,
        value: String,
    },
    Press {
        key: String,
    },
    Scroll {
        r#ref: Option<String>,
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
    },
    Wait {
        text: Option<String>,
        selector: Option<String>,
        #[serde(default)]
        idle: bool,
        timeout_secs: Option<u64>,
    },
}

impl Step {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::Fill { .. } => "fill",
            Self::Select { .. } => "select",
            Self::Press { .. } => "press",
            Self::Scroll { .. } => "scroll",
            Self::Wait { .. } => "wait",
        }
    }

    pub(super) fn ref_of(&self) -> Option<&str> {
        match self {
            Self::Click { r#ref } | Self::Fill { r#ref, .. } | Self::Select { r#ref, .. } => {
                Some(r#ref)
            }
            Self::Scroll { r#ref, .. } => r#ref.as_deref(),
            Self::Press { .. } | Self::Wait { .. } => None,
        }
    }

    /// The single tool this step is, and the exact input that tool would get.
    fn as_single(&self) -> (Op, Value) {
        match self {
            Self::Click { r#ref } => (Op::Click, json!({"ref": r#ref})),
            Self::Fill { r#ref, text } => (Op::Fill, json!({"ref": r#ref, "text": text})),
            Self::Select { r#ref, value } => (Op::Select, json!({"ref": r#ref, "value": value})),
            Self::Press { key } => (Op::Press, json!({"key": key})),
            Self::Scroll { r#ref, dx, dy } => {
                let mut v = json!({"dx": dx, "dy": dy});
                if let Some(r) = r#ref {
                    v["ref"] = json!(r);
                }
                (Op::Scroll, v)
            }
            Self::Wait {
                text,
                selector,
                idle,
                timeout_secs,
            } => {
                let mut v = json!({"idle": idle});
                if let Some(t) = text {
                    v["text"] = json!(t);
                }
                if let Some(s) = selector {
                    v["selector"] = json!(s);
                }
                if let Some(t) = timeout_secs {
                    v["timeout_secs"] = json!(t);
                }
                (Op::Wait, v)
            }
        }
    }

    /// One line naming the step. Typed text is never in it, only its length.
    fn describe(&self) -> String {
        match self {
            Self::Click { r#ref } => format!("click {}", sanitize_label(r#ref, 40)),
            Self::Fill { r#ref, text } => format!(
                "fill {} ({} characters)",
                sanitize_label(r#ref, 40),
                text.chars().count()
            ),
            Self::Select { r#ref, value } => format!(
                "select {} ({} characters)",
                sanitize_label(r#ref, 40),
                value.chars().count()
            ),
            Self::Press { key } => format!("press {}", sanitize_label(key, 40)),
            Self::Scroll { r#ref, dx, dy } => format!(
                "scroll {} ({dx},{dy})",
                r#ref
                    .as_deref()
                    .map_or_else(|| "page".to_owned(), |r| sanitize_label(r, 40))
            ),
            Self::Wait { .. } => "wait".to_owned(),
        }
    }
}

/// Parse and bound a batch. Used before the approval card is built AND before the
/// run, so what was approved is what is validated.
pub(super) fn parse_steps(input: &Value) -> Result<Vec<Step>, ToolError> {
    let parsed: ActIn = serde_json::from_value(input.clone()).map_err(|e| {
        ToolError::InvalidInput(format!(
            "{e}. browser_act takes {{\"steps\":[{{\"action\":\"click|fill|select|press|scroll|wait\", ...}}]}}; \
             open, eval, tabs, back, dialog options and secret fills are separate tool calls"
        ))
    })?;
    let steps = parsed.steps;
    if input.to_string().chars().count() > MAX_INPUT_CHARS {
        return Err(ToolError::InvalidInput(format!(
            "browser_act input is over {MAX_INPUT_CHARS} characters; split the batch (the approval must show every step in full)"
        )));
    }
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(ToolError::InvalidInput(format!(
            "browser_act takes 1 to {MAX_STEPS} steps, got {}",
            steps.len()
        )));
    }
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        if let Step::Wait {
            text,
            selector,
            idle,
            timeout_secs,
        } = step
        {
            let given =
                usize::from(text.is_some()) + usize::from(selector.is_some()) + usize::from(*idle);
            if given != 1 {
                return Err(ToolError::InvalidInput(format!(
                    "step {n}: a wait step needs exactly one of text, selector, idle"
                )));
            }
            if timeout_secs.is_some_and(|t| !(1..=MAX_WAIT_SECS).contains(&t)) {
                return Err(ToolError::InvalidInput(format!(
                    "step {n}: wait timeout_secs must be 1 to {MAX_WAIT_SECS} inside browser_act"
                )));
            }
        }
    }
    Ok(steps)
}

/// The ref the origin check of a batch starts from: the first step that names one.
pub(super) fn first_ref(input: &Value) -> Option<String> {
    parse_steps(input)
        .ok()?
        .iter()
        .find_map(|s| s.ref_of().map(str::to_owned))
}

/// Whether any step can put bytes into the page (fill, select, press). A batch of
/// only clicks, scrolls and waits cannot.
#[must_use]
pub fn can_egress(input: &Value) -> bool {
    match parse_steps(input) {
        Ok(steps) => steps.iter().any(|s| {
            matches!(
                s,
                Step::Fill { .. } | Step::Select { .. } | Step::Press { .. }
            )
        }),
        // Unreadable input is judged by the strictest rule.
        Err(_) => true,
    }
}

/// The one-line summary of an approval: EVERY step, in order, without any typed text.
/// `None` when the input is not a valid batch (the caller falls back to the raw input).
#[must_use]
pub fn summary(input: &Value) -> Option<String> {
    let steps = parse_steps(input).ok()?;
    let lines: Vec<String> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s.describe()))
        .collect();
    Some(format!("{} steps: {}", steps.len(), lines.join("; ")))
}

/// Why a batch stopped, and the steps that did not run.
fn report(steps: &[Step], done: usize, stop: Option<&str>, last: Option<&str>) -> String {
    let mut out = String::new();
    match stop {
        None => {
            let _ = writeln!(out, "browser_act: done {done}/{}", steps.len());
        }
        Some(why) => {
            let _ = writeln!(
                out,
                "browser_act: stopped, done {done}/{}: {why}",
                steps.len()
            );
        }
    }
    for (i, step) in steps.iter().enumerate().take(done) {
        let _ = writeln!(out, "{}. {} ok", i + 1, step.describe());
    }
    if done < steps.len() {
        let _ = writeln!(out, "not run:");
        for (i, step) in steps.iter().enumerate().skip(done) {
            let _ = writeln!(out, "{}. {}", i + 1, step.describe());
        }
        let _ = writeln!(
            out,
            "Nothing after the stop was done. Look at the page state below or take a browser_snapshot before continuing."
        );
    }
    if let Some(text) = last {
        let _ = write!(out, "\n{text}");
    }
    out
}

impl BrowserTool {
    /// Run a batch. Each step is exactly its single tool; see the module docs.
    pub(super) async fn run_act(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let steps = parse_steps(&input)?;
        let d = Arc::clone(&self.shared.driver);
        // Fingerprint of every ref, taken before anything runs. A ref the driver cannot
        // vouch for refuses the whole batch: nothing was done.
        let mut prints: HashMap<String, (String, String)> = HashMap::new();
        for r in steps.iter().filter_map(Step::ref_of) {
            if prints.contains_key(r) {
                continue;
            }
            let Some(id) = d.ref_identity(r).await else {
                return Err(exec_err(format!(
                    "browser_act: ref {} is unknown or cannot be verified; nothing was done. Take a browser_snapshot and use its refs",
                    sanitize_label(r, 40)
                )));
            };
            prints.insert(r.to_owned(), id);
        }
        let start_url = d.current_url().await;
        let deadline = Instant::now() + TOTAL_BUDGET;
        let expected = expected_input_origin();
        let mut done = 0usize;
        let mut last: Option<String> = None;
        let mut stop: Option<String> = None;
        for (i, step) in steps.iter().enumerate() {
            let n = i + 1;
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                stop = Some(format!(
                    "step {n} not started: the {}s batch time budget is used up",
                    TOTAL_BUDGET.as_secs()
                ));
                break;
            };
            if let Some(r) = step.ref_of() {
                let now = d.ref_identity(r).await;
                if now.as_ref() != prints.get(r) {
                    stop = Some(format!(
                        "step {n} not run: the page changed, {} is no longer the element it was (was {}, now {})",
                        sanitize_label(r, 40),
                        show_identity(prints.get(r)),
                        show_identity(now.as_ref())
                    ));
                    break;
                }
            }
            if start_url.is_some() && d.current_url().await != start_url {
                stop = Some(format!("step {n} not run: the page navigated away"));
                break;
            }
            if let Some(expected) = &expected {
                let at = d
                    .input_origin(step.ref_of())
                    .await
                    .and_then(|o| origin_of(&o));
                if at.as_deref() != Some(expected.as_str()) {
                    stop = Some(format!(
                        "step {n} not run: the receiving frame is not on {expected} any more (it is on {})",
                        at.as_deref().unwrap_or("an unknown page")
                    ));
                    break;
                }
            }
            let (op, single) = step.as_single();
            let tool = BrowserTool {
                spec: self.spec.clone(),
                op,
                shared: Arc::clone(&self.shared),
            };
            let text = match tokio::time::timeout(
                remaining,
                Box::pin(tool.run_inner(run_id, single)),
            )
            .await
            {
                Err(_) => {
                    stop = Some(format!(
                        "step {n} ({}) was cut off when the {}s batch time budget ran out; it may or may not have been done",
                        step.name(),
                        TOTAL_BUDGET.as_secs()
                    ));
                    break;
                }
                // Same error, same wording, as the single tool when nothing ran yet.
                Ok(Err(e)) if i == 0 => return Err(e),
                Ok(Err(e)) => {
                    stop = Some(format!("step {n} ({}) failed: {e}", step.name()));
                    break;
                }
                Ok(Ok(text)) => text,
            };
            if text.starts_with("Human intervention completed")
                || text == crate::control::OBSERVATION_WITHHELD
            {
                // Discarded or withheld because a human took over: not performed.
                stop = Some(format!(
                    "step {n} ({}) was not performed: control changed",
                    step.name()
                ));
                last = Some(text);
                break;
            }
            done += 1;
            let dialog = text.contains("~ dialog(");
            last = Some(text);
            if dialog {
                stop = Some(format!(
                    "a dialog appeared at step {n}; look at it before going on"
                ));
                break;
            }
            if start_url.is_some() && d.current_url().await != start_url {
                stop = Some(format!("step {n} navigated the page"));
                break;
            }
        }
        Ok(report(&steps, done, stop.as_deref(), last.as_deref()))
    }

    /// The approval card details of a batch: EVERY step, in order, with what its ref is now.
    pub(super) async fn act_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let steps = parse_steps(input)?;
        // The bot's own browser (and run mode), the one the steps will run on.
        let driver = Arc::clone(&self.select_shared(ctx)?.driver);
        let mut rows = Vec::with_capacity(steps.len());
        for (i, step) in steps.iter().enumerate() {
            let target = match step.ref_of() {
                Some(r) => match driver.ref_identity(r).await {
                    Some((role, name)) => format!("{role} \"{}\"", sanitize_label(&name, 80)),
                    None => "unknown element".to_owned(),
                },
                None => String::new(),
            };
            let mut row = json!({
                "n": i + 1,
                "action": step.name(),
                "ref": step.ref_of().map(|r| sanitize_label(r, 40)),
                "target": target,
            });
            match step {
                Step::Fill { text, .. } => {
                    row["text"] = json!(sanitize_label(text, 300));
                    row["text_chars"] = json!(text.chars().count());
                }
                Step::Select { value, .. } => row["value"] = json!(sanitize_label(value, 120)),
                Step::Press { key } => row["key"] = json!(sanitize_label(key, 40)),
                Step::Wait {
                    text,
                    selector,
                    idle,
                    ..
                } => {
                    row["wait_for"] = json!(if *idle {
                        "network idle".to_owned()
                    } else {
                        sanitize_label(text.as_deref().or(selector.as_deref()).unwrap_or(""), 120)
                    });
                }
                Step::Click { .. } | Step::Scroll { .. } => {}
            }
            rows.push(row);
        }
        Ok(json!({
            "kind": "browser_steps",
            "tool": "browser_act",
            "steps": rows,
            "note": "Every step is checked against the page before it runs; the batch stops at the first change.",
        }))
    }
}

fn show_identity(id: Option<&(String, String)>) -> String {
    id.map_or_else(
        || "gone".to_owned(),
        |(role, name)| format!("{role} \"{}\"", sanitize_label(name, 60)),
    )
}

#[cfg(test)]
#[path = "act_tests.rs"]
mod act_tests;
