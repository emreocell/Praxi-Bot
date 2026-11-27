//! A failed browser step keeps the page: the model gets the page's current state and is told
//! to recover in place, not to open the address again.
//!
//! Why: a form half filled in is lost the moment the page is loaded again, and a model that
//! only sees "element not found" tends to do exactly that. When a step on an open page fails
//! the error is followed by what the browser shows now (the interactive elements with their
//! values, a screenshot) and by the ways to carry on without a reload. Repeating the same
//! failing step is called out and pointed at another way (pointer, the owner).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tool_registry::ToolError;

use super::{BrowserTool, Op};

/// Longest page state shown after a failure, in lines.
const MAX_STATE_LINES: usize = 60;
/// Longest one line of the state, in characters.
const MAX_LINE_CHARS: usize = 220;
/// Time the state may take to read; a failure never turns into a hang.
const STATE_BUDGET: Duration = Duration::from_secs(8);
/// Runs whose failure streak is remembered.
const MAX_STREAKS: usize = 64;
/// A streak nobody touched for this long is forgotten (a run that ended mid-failure).
const STREAK_TTL: Duration = Duration::from_secs(30 * 60);

/// Opens the part of a failed step's error that is the PAGE (its address, its controls, a
/// picture). The agent loop frames that part like any other page text, through the quarantined
/// reader for a run that holds private data; the words around it are the daemon's own.
pub const RECOVERY_OPEN: &str = "[[praxi:page-state]]";
/// Closes what [`RECOVERY_OPEN`] opened.
pub const RECOVERY_CLOSE: &str = "[[praxi:end-page-state]]";

/// `text` with every look-alike of the two markers broken, so a page (or an error that quotes
/// one) cannot end the page part early and have its words read as the daemon's advice.
pub(super) fn defuse(text: &str) -> String {
    text.replace("[[praxi:", "[ [praxi:")
}

/// A failed step's error split at its page part: the tool's own words, the page, the advice.
/// `None` when the error has no page part (the step failed before the page was read).
#[must_use]
pub fn split_recovery(text: &str) -> Option<(&str, &str, &str)> {
    let open = text.find(RECOVERY_OPEN)?;
    let after = open + RECOVERY_OPEN.len();
    let close = after + text[after..].find(RECOVERY_CLOSE)?;
    Some((
        text[..open].trim_end(),
        text[after..close].trim(),
        text[close + RECOVERY_CLOSE.len()..].trim_start(),
    ))
}

/// What identifies a step for the failure streak: the tool and its input, with a pointer
/// action's coordinates rounded to a 16 pixel grid (the same click one pixel over is the same
/// failing step) and the whole thing cut short.
#[must_use]
pub(super) fn step_key(op: Op, input: &serde_json::Value) -> String {
    fn coarse(v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::Number(n) => n.as_f64().map_or_else(
                || v.clone(),
                #[allow(clippy::cast_possible_truncation)] // a coordinate, far below i64's range
                |f| serde_json::json!(((f / 16.0).round() * 16.0) as i64),
            ),
            Value::Array(a) => Value::Array(a.iter().map(coarse).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), coarse(v))).collect())
            }
            other => other.clone(),
        }
    }
    let shown = if op == Op::Pointer {
        coarse(input)
    } else {
        input.clone()
    };
    format!(
        "{op:?} {}",
        shown.to_string().chars().take(300).collect::<String>()
    )
}

/// Failing the same step this many times in a row calls for another approach.
pub(super) const SWITCH_AFTER: u32 = 2;
/// ... and this many for telling the owner instead of trying on.
pub(super) const GIVE_UP_AFTER: u32 = 3;

/// Whether `op` is a step on an open page (a failure there should keep the page).
pub(super) fn is_page_step(op: Op) -> bool {
    matches!(
        op,
        Op::Click
            | Op::Fill
            | Op::Select
            | Op::Press
            | Op::Scroll
            | Op::Hover
            | Op::SelectText
            | Op::DismissConsent
            | Op::Act
            | Op::Pointer
            | Op::Upload
    )
}

/// Whether an error is a step that failed on the page, as opposed to a refusal, an approval
/// or a hand-over matter where reading the page and retrying would be wrong.
pub(super) fn is_recoverable(err: &ToolError) -> bool {
    let ToolError::Execution(msg) = err else {
        return false;
    };
    let low = msg.to_lowercase();
    ![
        "browser_handover",
        "captcha",
        "refused",
        "human intervention",
        "human takeover",
        "observation",
        "approval",
        "denied",
        "unavailable",
        "browser_idle",
        "no secrets",
        "[praxi notice] the step failed",
        // The trust checks of the pointer and typing paths: another origin, a frame that
        // changed under the point, focus that moved. Reading the page and trying again
        // would be asking the same refused question.
        "origin changed",
        "receiving frame",
        "no longer on",
        "focus moved",
        "under the pointer changed",
        "nothing was pressed",
        "nothing was typed",
        "was not typed",
        "not on this computer",
        "another frame than it starts in",
    ]
    .iter()
    .any(|m| low.contains(m))
}

/// How many times in a row the same step has now failed for this run.
fn streak(run_id: &str, step: &str) -> u32 {
    let mut map = streaks();
    let now = Instant::now();
    if map.len() >= MAX_STREAKS && !map.contains_key(run_id) {
        // Forget what nobody touched for a while; when that is not enough, the longest idle.
        map.retain(|_, v| now.duration_since(v.2) < STREAK_TTL);
        if map.len() >= MAX_STREAKS
            && let Some(oldest) = map.iter().min_by_key(|(_, v)| v.2).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    let entry = map
        .entry(run_id.to_owned())
        .or_insert_with(|| (String::new(), 0, now));
    if entry.0 == step && now.duration_since(entry.2) < STREAK_TTL {
        entry.1 += 1;
    } else {
        step.clone_into(&mut entry.0);
        entry.1 = 1;
    }
    entry.2 = now;
    entry.1
}

/// A step that worked ends its run's streak.
pub(super) fn forget_streak(run_id: &str) {
    streaks().remove(run_id);
}

type Streaks = HashMap<String, (String, u32, Instant)>;

fn streaks() -> std::sync::MutexGuard<'static, Streaks> {
    static STREAKS: OnceLock<Mutex<Streaks>> = OnceLock::new();
    STREAKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The advice that follows the state, by how often this step has failed.
pub(super) fn advice(failures: u32) -> String {
    let mut out = String::from(
        "[Praxi notice] The step failed, but the page is still open exactly as it was: nothing was reloaded and what you typed or chose is kept. Do NOT call browser_open on this address again. Read the state above and recover IN PLACE:\n\
         - a ref went stale or is unknown: use the refs listed above (browser_snapshot for the full page);\n\
         - the element is covered, hidden or off screen: browser_scroll {\"ref\":...,\"into_view\":true}, close the overlay (browser_dismiss_consent for a cookie banner), then try again;\n\
         - a field lost its value: browser_fill that one field again, not the whole form;\n\
         - no ref for it (a widget in another site's frame, a canvas, a custom control): browser_pointer on the screenshot;\n\
         - the page really is broken: browser_open with reload:true, on purpose, and redo the form.",
    );
    if failures >= GIVE_UP_AFTER {
        let _ = write!(
            out,
            "\nThis same step has now failed {failures} times in a row. Stop repeating it: try a different way once, and if that fails too tell the owner what blocked you (or mark this item blocked and move on) instead of trying again."
        );
    } else if failures >= SWITCH_AFTER {
        let _ = write!(
            out,
            "\nThis same step has now failed {failures} times in a row: do not send it again unchanged. Change something: another ref, scroll it into view first, or browser_pointer at the screenshot."
        );
    }
    out
}

/// At most [`MAX_STATE_LINES`] lines of `state`, each cut short.
pub(super) fn cap_state(state: &str) -> String {
    let lines: Vec<&str> = state.lines().collect();
    let mut out = String::new();
    for line in lines.iter().take(MAX_STATE_LINES) {
        let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
        out.push_str(&cut);
        if line.chars().count() > MAX_LINE_CHARS {
            out.push('\u{2026}');
        }
        out.push('\n');
    }
    if lines.len() > MAX_STATE_LINES {
        let _ = writeln!(
            out,
            "... {} more lines: browser_snapshot shows the whole page",
            lines.len() - MAX_STATE_LINES
        );
    }
    out
}

impl BrowserTool {
    /// `err` followed by the page's state and the way to recover, when the failure is a step on
    /// an open page. The error is returned unchanged when the page cannot be read.
    pub(super) async fn with_recovery(
        &self,
        run_id: &str,
        step: &str,
        err: ToolError,
    ) -> ToolError {
        if !is_page_step(self.op) || !is_recoverable(&err) {
            return err;
        }
        let ToolError::Execution(msg) = &err else {
            return err;
        };
        let d = &self.shared.driver;
        // Never starts a browser: no page, nothing to keep.
        let Some(url) = d.current_url().await else {
            return err;
        };
        if self.refuse_observation_while_latched().is_err() {
            return err;
        }
        let failures = streak(run_id, step);
        let gathered = tokio::time::timeout(STATE_BUDGET, async {
            let state = d.snapshot(true).await.ok();
            let shot = self.fresh_shot(d).await;
            (state, shot)
        })
        .await;
        // The page part is everything the page could have written: its address, its controls
        // and the line naming the picture. It sits between two markers the loop frames.
        let mut page = format!("page now: {}", defuse(&url));
        if let Ok((state, shot)) = gathered {
            if let Some(state) = state {
                let _ = write!(page, "\n{}", defuse(&cap_state(&state)));
            }
            page.push_str(&defuse(&shot));
        }
        let mut out = defuse(msg);
        let _ = write!(out, "\n{RECOVERY_OPEN}\n{page}\n{RECOVERY_CLOSE}");
        let _ = write!(out, "\n{}", advice(failures));
        ToolError::Execution(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_steps_that_failed_on_the_page_keep_the_page() {
        let exec = |m: &str| ToolError::Execution(m.to_owned());
        assert!(is_recoverable(&exec("unknown ref e12")));
        assert!(is_recoverable(&exec("another element is on top of 'Go'")));
        // Refusals, hand-over matters and approvals are not retried by reading the page.
        for m in [
            "refused: the target of this key press is, or may be, a password field",
            "CAPTCHA scripts cannot be bounded to two attempts: hand over with browser_handover(kind: captcha)",
            "browser_idle; call browser_open before reading the browser",
            "observation refused while a typed one-time code is visible",
            "browser is unavailable for this run's computer target",
        ] {
            assert!(!is_recoverable(&exec(m)), "{m}");
        }
        assert!(!is_recoverable(&ToolError::InvalidInput("x".into())));
        assert!(is_page_step(Op::Click) && is_page_step(Op::Pointer) && is_page_step(Op::Act));
        assert!(!is_page_step(Op::Open) && !is_page_step(Op::Snapshot) && !is_page_step(Op::Eval));
    }

    #[test]
    fn the_state_is_capped_by_lines_and_by_width() {
        let long = "x".repeat(500);
        let state = format!("{long}\n{}", "[e1] button \"Go\"\n".repeat(100));
        let c = cap_state(&state);
        assert_eq!(c.lines().count(), MAX_STATE_LINES + 1);
        assert!(c.lines().next().unwrap().chars().count() <= MAX_LINE_CHARS + 1);
        assert!(c.contains("more lines: browser_snapshot"), "{c}");
        assert!(!cap_state("one\ntwo").contains("more lines"));
    }

    #[test]
    fn repeating_a_failure_escalates_and_a_different_step_starts_over() {
        let run = "run-recover-test";
        assert_eq!(streak(run, "click e1"), 1);
        assert_eq!(streak(run, "click e1"), 2);
        assert_eq!(streak(run, "click e1"), 3);
        assert_eq!(streak(run, "click e2"), 1);
        forget_streak(run);
        assert_eq!(streak(run, "click e2"), 1);
        let first = advice(1);
        assert!(first.contains("Do NOT call browser_open"), "{first}");
        assert!(first.contains("browser_pointer"), "{first}");
        assert!(!first.contains("in a row"));
        assert!(advice(SWITCH_AFTER).contains("do not send it again unchanged"));
        let third = advice(GIVE_UP_AFTER);
        assert!(third.contains("tell the owner"), "{third}");
        assert!(third.contains("mark this item blocked"), "{third}");
    }
}
