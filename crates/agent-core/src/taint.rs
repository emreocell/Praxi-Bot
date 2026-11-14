//! What a run has already put in front of the model, and the card shown when
//! that run then wants to send data off the machine.
//!
//! Bits are only ever set. A later tool, a subagent, or a resume of the same
//! run can add sources; nothing clears them.

use permission_engine::ContentExposure;
use serde_json::{Value, json};
use tool_registry::ContentClass;

/// How many source labels an approval card lists. Older reads stay in the
/// bits; the card names the most recent distinct ones that still fit.
const MAX_SOURCES: usize = 8;

/// Exposure bits plus the labels the approval card quotes.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentTaint {
    pub exposure: ContentExposure,
    sources: Vec<String>,
}

impl ContentTaint {
    /// Record that a tool result of this class entered the context.
    ///
    /// A class that neither ingests untrusted content nor reads private data
    /// changes nothing. The same label is stored once.
    pub(crate) fn note(&mut self, class: ContentClass, label: &str) {
        if !class.ingests_untrusted && !class.reads_private {
            return;
        }
        self.exposure
            .observe(class.ingests_untrusted, class.reads_private);
        self.push_label(label);
    }

    /// Union with taint inherited from a parent run or from stored history.
    pub(crate) fn inherit(&mut self, other: &Self) {
        self.exposure.inherit(other.exposure);
        for label in &other.sources {
            self.push_label(label);
        }
    }

    fn push_label(&mut self, label: &str) {
        let label = label.trim();
        if label.is_empty() || self.sources.iter().any(|s| s == label) {
            return;
        }
        if self.sources.len() >= MAX_SOURCES {
            return;
        }
        self.sources.push(label.to_owned());
    }
}

/// Fold one persisted step into `taint`.
///
/// `class_of` resolves the tool that actually ran (`use_tool` included) to its
/// declared class. A missing tool is skipped: it cannot be shown to have
/// returned content. Permission denials are skipped too; a tool error after
/// the call started is not, because the result text is already in the step.
pub(crate) fn observe_history_step(
    taint: &mut ContentTaint,
    name: &str,
    input: &Value,
    output: Option<&Value>,
    class_of: &mut dyn FnMut(&str, &Value) -> Option<ContentClass>,
) {
    match name {
        "user_message" => {
            let text = input["text"].as_str().unwrap_or_default();
            let recalled = text.contains("<recalled_memory>");
            let untrusted = text.contains("<untrusted_memory_data>");
            if recalled || untrusted {
                taint.note(ContentClass::declared(untrusted, true, false), "memory");
            }
        }
        "workflow" => {
            let Some(out) = output else { return };
            let note = out["note"].as_str().unwrap_or_default();
            // The early "could not be replayed" failure never put a report in
            // front of the model.
            if note.starts_with("The saved workflow could not be replayed") {
                return;
            }
            if out.get("summary").is_some() || out.get("final_text").is_some() {
                taint.note(ContentClass::untrusted_private(), "workflow replay");
            }
        }
        "tools" => {
            let calls = input["calls"].as_array().cloned().unwrap_or_default();
            let results = output
                .and_then(|o| o["results"].as_array())
                .cloned()
                .unwrap_or_default();
            for call in calls {
                let id = call["id"].as_str().unwrap_or_default();
                let Some(result) = results.iter().find(|r| r["id"].as_str() == Some(id)) else {
                    continue;
                };
                let text = result["text"].as_str().unwrap_or_default();
                if !ran_result(text) {
                    continue;
                }
                let tool = call["name"].as_str().unwrap_or_default();
                let args = call["arguments"].as_str().unwrap_or("{}");
                let parsed: Value = serde_json::from_str(args).unwrap_or(Value::Null);
                let Some(class) = class_of(tool, &parsed) else {
                    continue;
                };
                // The closure resolves `use_tool` to the target. Ask it for the
                // name it classified so the label matches that tool.
                let (eff_name, eff_input) = effective_call(tool, &parsed);
                taint.note(class, &source_label(&eff_name, &eff_input));
            }
        }
        _ => {}
    }
}

/// A stored result that means the tool actually started.
///
/// Policy and user denials never put tool output in context. An `Error:` from
/// the tool itself did run, so it still taints.
fn ran_result(text: &str) -> bool {
    let t = text.trim_start();
    if t.starts_with("Permission denied") {
        return false;
    }
    if t.starts_with("Error: unknown tool") || t.starts_with("Error: invalid JSON") {
        return false;
    }
    if t.starts_with("[interrupted:") {
        return false;
    }
    true
}

/// `use_tool` hides the target name and input. Everyone else is itself.
fn effective_call(name: &str, input: &Value) -> (String, Value) {
    if name == "use_tool" {
        let target = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned();
        let inner = input.get("input").cloned().unwrap_or_else(|| json!({}));
        return (target, inner);
    }
    (name.to_owned(), input.clone())
}

/// Short label for the approval card: `file <path>`, `mailbox <folder>`,
/// `page <url>`, and the same shape for the other sources.
pub(crate) fn source_label(name: &str, input: &Value) -> String {
    if let Some(server) = name.strip_prefix("mcp:") {
        let server = server.split('/').next().unwrap_or("mcp");
        return format!("mcp {server}");
    }
    let str_field = |key: &str| input.get(key).and_then(Value::as_str);
    match name {
        "fs_read" | "fs_list" | "fs_search" | "fs_write" | "fs_edit" | "fs_move" | "fs_delete"
        | "checkpoint_list" | "checkpoint_restore" | "pdf_extract" | "docx_read" | "docx_write"
        | "xlsx_read" | "xlsx_write" | "csv_read" | "csv_transform" | "json_query"
        | "git_status" | "git_diff" | "git_log" | "git_show" | "git_branch" | "git_commit" => {
            format!("file {}", str_field("path").unwrap_or("a file"))
        }
        "email_read" | "email_search" | "email_list_folders" => {
            format!("mailbox {}", str_field("folder").unwrap_or("INBOX"))
        }
        "web_search" => format!("page {}", str_field("query").unwrap_or("a search")),
        "web_extract" | "web_feed" | "web_sitemap" | "web_metadata" | "browser_open"
        | "browser_back" | "browser_snapshot" | "browser_screenshot" | "browser_click"
        | "browser_fill" | "browser_select" | "browser_press" | "browser_scroll"
        | "browser_wait" | "browser_tabs" | "browser_eval" => {
            format!("page {}", str_field("url").unwrap_or("the browser"))
        }
        "skill_read" | "skill_list" | "skill_write" | "skill_publish" => {
            format!("file skill {}", str_field("name").unwrap_or("a skill"))
        }
        "workflow_run" | "workflow_list" => "workflow replay".to_owned(),
        other if other.starts_with("desktop_") => format!(
            "window {}",
            str_field("title")
                .or_else(|| str_field("executable"))
                .unwrap_or("desktop")
        ),
        "process_list" => "window desktop".to_owned(),
        _ => format!("file {name}"),
    }
}

/// Destination host, the exact outgoing text, and which kind of text it is.
pub(crate) fn egress_payload(name: &str, input: &Value) -> (String, String, &'static str) {
    let str_field = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if let Some(server) = name.strip_prefix("mcp:") {
        let server = server.split('/').next().unwrap_or("mcp").to_owned();
        return (server, input.to_string(), "body");
    }
    match name {
        "web_search" => (
            search_host(input.get("backend").and_then(Value::as_str)),
            str_field("query"),
            "query",
        ),
        "web_extract" | "web_feed" | "web_sitemap" | "web_metadata" | "browser_open" => {
            let url = str_field("url");
            let host = host_of_url(&url);
            (host, url, "url")
        }
        "email_send" | "email_draft" => {
            let to = first_addr(input.get("to"));
            let host = domain_of_addr(&to);
            let body = format!(
                "To: {to}\nSubject: {}\n\n{}",
                str_field("subject"),
                str_field("body")
            );
            (host, body, "body")
        }
        "shell_run" => ("the shell".to_owned(), str_field("command"), "body"),
        "script_run" => ("a script".to_owned(), str_field("code"), "body"),
        "browser_fill" | "browser_type" => ("the browser".to_owned(), str_field("text"), "body"),
        "browser_eval" => ("the browser".to_owned(), str_field("js"), "body"),
        "schedule_create" | "schedule_update" | "schedule_resume" => (
            "this bot's scheduler".to_owned(),
            str_field("prompt"),
            "body",
        ),
        _ => {
            if let Some(url) = input.get("url").and_then(Value::as_str) {
                (host_of_url(url), url.to_owned(), "url")
            } else {
                (name.to_owned(), input.to_string(), "body")
            }
        }
    }
}

fn search_host(backend: Option<&str>) -> String {
    match backend {
        Some("duckduckgo") => "html.duckduckgo.com".to_owned(),
        Some("brave") => "api.search.brave.com".to_owned(),
        Some("searxng") => "searxng".to_owned(),
        _ => "configured-search-provider".to_owned(),
    }
}

fn host_of_url(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        "unknown".to_owned()
    } else {
        host.to_owned()
    }
}

fn first_addr(value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(a)) => a.first().and_then(Value::as_str).unwrap_or("").to_owned(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn domain_of_addr(addr: &str) -> String {
    let addr = addr.trim();
    let addr = addr
        .rfind('<')
        .map_or(addr, |i| addr[i + 1..].trim_end_matches('>').trim());
    addr.split('@')
        .nth(1)
        .filter(|d| !d.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

/// JSON merged onto an approval card when a tainted run calls an egress tool.
pub(crate) fn egress_details(taint: &ContentTaint, name: &str, input: &Value) -> Option<Value> {
    if !taint.exposure.is_tainted() {
        return None;
    }
    let (host, payload, payload_kind) = egress_payload(name, input);
    let read: Vec<&str> = taint.sources.iter().map(String::as_str).collect();
    let sources = if read.is_empty() {
        "untrusted or private content".to_owned()
    } else {
        read.join(", ")
    };
    let reason = format!("this run read {sources} and now wants to send data to {host}");
    Some(json!({
        "kind": "egress",
        "reason": reason,
        "read": read,
        "host": host,
        "payload": payload,
        "payload_kind": payload_kind,
    }))
}

/// Keep a tool's own card (schedule, and anything else with a `kind`) and put
/// the egress block beside it. A card with no details becomes the egress card.
pub(crate) fn merge_egress(details: &mut Option<Value>, egress: Value) {
    match details {
        None => *details = Some(egress),
        Some(value) => {
            if value.get("kind").and_then(Value::as_str) == Some("egress") {
                return;
            }
            if let Some(obj) = value.as_object_mut() {
                obj.insert("egress".to_owned(), egress);
            } else {
                *details = Some(egress);
            }
        }
    }
}
