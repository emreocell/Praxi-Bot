//! `browser_attach`: use the browser the owner already has open.
//!
//! "Use this one" means the owner's own Chrome or Edge, started with remote debugging on this
//! computer, not a second window of the assistant's. After a successful attach every browser
//! tool drives that browser (its visible tab, its tabs, its signed-in sessions) until
//! `detach`. Because such a browser holds the owner's accounts:
//!
//! * the call is confirmed on EVERY use (`always_ask_for`), however permissive the policy;
//! * only an endpoint on this computer is connected to (`browser_cdp::attach`);
//! * a computer that is a paired server cannot attach to the owner's machine;
//! * what it shows counts as the owner's private data (`signed-in browser`, see
//!   [`owner_exposure`]), so the page reader and the egress rules treat the run accordingly;
//! * every browser result names the owner's browser while it is in use ([`browser_line`]), which
//!   is how the run timeline shows it.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, OutputExposure, PermissionCategory, ToolError, ToolSpec};

use super::{BrowserDriver, BrowserTool};
use crate::handover::Reach;
use crate::util::{exec_err, parse, spec};

/// `browser_attach` input.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachIn {
    endpoint: Option<String>,
    #[serde(default)]
    detach: bool,
}

impl AttachIn {
    /// Checked before anything is asked or connected.
    pub(super) fn checked(self) -> Result<Self, ToolError> {
        if self.detach && self.endpoint.is_some() {
            return Err(ToolError::InvalidInput(
                "give either `endpoint` (attach) or `detach`, not both".into(),
            ));
        }
        if let Some(e) = &self.endpoint {
            browser_cdp::attach::loopback_endpoint(e)
                .map_err(|err| ToolError::InvalidInput(err.to_string()))?;
        }
        Ok(self)
    }
}

/// Whether this input asks to attach (as opposed to detach): the always-ask test.
pub(super) fn attaches(input: &Value) -> bool {
    !input
        .get("detach")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn tool_spec(category: PermissionCategory) -> ToolSpec {
    spec(
        "browser_attach",
        "Use the browser the OWNER already has open instead of the assistant's own: call it BEFORE browser_open when the owner says \"use this browser\" or \"my open Chrome\". Needs Chrome or Edge on this computer started with --remote-debugging-port=9222; leave `endpoint` out to look at the usual places. Every browser_* tool then drives it (their visible tab first). The owner confirms each attach; what it shows is private to them. {\"detach\":true} goes back to the assistant's own browser. Example: {\"endpoint\":\"http://127.0.0.1:9222\"}",
        json!({"type":"object","properties":{
            "endpoint":{"type":"string","description":"http://127.0.0.1:9222 or a ws:// address on this computer."},
            "detach":{"type":"boolean","description":"Back to the assistant's own browser. Not with `endpoint`."}
        }}),
        Effect::Write,
        category,
    )
}

/// The card line for an attach request.
pub(super) fn card_line(input: &Value) -> String {
    let at = input
        .get("endpoint")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
        .unwrap_or("the usual local ports");
    format!(
        "attach to the owner's own browser at {at}: every tab it has open and every site it is signed in to becomes visible to and drivable by this bot"
    )
}

/// The line a browser result carries while the owner's browser is attached: which browser it
/// came from. (The assistant's own browser needs no line on every call.)
pub(super) fn browser_line(endpoint: &str) -> String {
    format!(
        "[browser: the OWNER's own browser, attached at {endpoint}; what it shows is private to the owner]"
    )
}

/// What a browser result is, while the owner's browser is attached: untrusted page text that
/// is also the owner's private data (the same class as a browser profile the owner signed in).
pub(super) fn owner_exposure() -> OutputExposure {
    OutputExposure {
        ingests_untrusted: true,
        reads_private: true,
        label: "signed-in browser".to_owned(),
        trust_folder: None,
    }
}

impl BrowserTool {
    /// `browser_attach`: gate, connect (or disconnect) and say what the model now drives.
    pub(super) async fn run_attach(&self, run_id: &str, input: Value) -> Result<String, ToolError> {
        let i = parse::<AttachIn>(input)?.checked()?;
        if self
            .shared
            .control
            .as_ref()
            .is_some_and(|g| g.remote_browser_for_run(run_id).is_some())
        {
            return Err(exec_err(
                "this bot's computer is a paired server: its browser cannot attach to a browser on the owner's own computer. Use the server's browser, or ask the owner to run the task from their own computer",
            ));
        }
        let d: Arc<dyn BrowserDriver> = Arc::clone(&self.shared.driver);
        if i.detach {
            return d.detach().await.map_err(exec_err);
        }
        let permit = self
            .gate(
                run_id,
                "open",
                "the owner's own browser".to_owned(),
                Reach::Nothing,
            )
            .await?;
        if let Some(fresh) = self.discard_if_stale(&permit, &d).await? {
            return Ok(fresh);
        }
        let endpoint = i.endpoint;
        let what = {
            let d = Arc::clone(&d);
            permit
                .protect(async move { d.attach(endpoint.as_deref()).await })
                .await
                .map_err(exec_err)?
        }
        .map_err(exec_err)?;
        let mut out = format!(
            "{what}.\nFrom now on every browser_* tool drives this browser, starting with the tab the owner is looking at. browser_open opens a page in it; browser_tabs lists and switches the owner's tabs; browser_attach with detach:true goes back to the assistant's own browser. What it shows is private to the owner."
        );
        if self.refuse_observation_while_latched().is_ok() {
            // Only HOW MANY tabs: the others' addresses (query tokens, sites the owner blocked
            // for this bot) are not volunteered; browser_tabs lists them, addresses trimmed.
            if let Ok(tabs) = d.tabs(None).await {
                let n = tabs.lines().count();
                let _ = write!(
                    out,
                    "\n{n} tab(s) are open in it; browser_tabs lists them (without query strings)."
                );
            }
            if let Ok(page) = d.snapshot(true).await {
                let _ = write!(
                    out,
                    "\nThe tab in use:\n{}",
                    super::recover::cap_state(&page)
                );
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_checked_before_anything_is_asked_or_connected() {
        let ok = |v: Value| parse::<AttachIn>(v).and_then(AttachIn::checked);
        assert!(ok(json!({})).is_ok());
        assert!(ok(json!({"endpoint":"http://127.0.0.1:9222"})).is_ok());
        assert!(ok(json!({"detach":true})).is_ok());
        assert!(ok(json!({"detach":true,"endpoint":"127.0.0.1:9222"})).is_err());
        // Never a debugging port on another machine, whatever the model was told.
        assert!(ok(json!({"endpoint":"http://192.168.1.20:9222"})).is_err());
        assert!(ok(json!({"endpoint":"http://evil.example:9222"})).is_err());
        assert!(ok(json!({"url":"http://127.0.0.1:9222"})).is_err());
    }

    #[test]
    fn attaching_is_asked_every_time_detaching_is_not() {
        assert!(attaches(&json!({})));
        assert!(attaches(&json!({"endpoint":"127.0.0.1:9222"})));
        assert!(!attaches(&json!({"detach":true})));
    }

    #[test]
    fn every_result_names_the_browser_in_use() {
        let line = browser_line("http://127.0.0.1:9222");
        assert!(line.contains("OWNER's own browser"), "{line}");
        assert!(line.contains("http://127.0.0.1:9222"), "{line}");
        assert!(card_line(&json!({})).contains("usual local ports"));
        assert!(card_line(&json!({"endpoint":"http://127.0.0.1:9333"})).contains("9333"));
    }

    #[test]
    fn the_owner_browser_counts_as_private_untrusted_content() {
        let e = owner_exposure();
        assert!(e.ingests_untrusted && e.reads_private);
        assert_eq!(e.label, "signed-in browser");
    }
}
