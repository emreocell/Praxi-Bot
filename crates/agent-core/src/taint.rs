//! What a run has already put in front of the model, and the card shown when
//! that run then wants to send data off the machine.
//!
//! Bits are only ever set. A later tool, a subagent, or a resume of the same
//! run can add sources; nothing clears them.
//!
//! Whether a tool ran, and which class of content that put in context, is a
//! fact the daemon records when the call happens. Rebuilding a run reads those
//! facts. It does not look at the tool list the model can see right now, and
//! it does not look at the bytes the tool returned.

use permission_engine::ContentExposure;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_registry::{ContentClass, OutputExposure};

/// Trusted record of one execution, stored with the step that produced it.
///
/// `ran` is true only when the daemon started the tool and kept the result.
/// A denial, an unknown name, or invalid arguments are `ran: false`. The
/// result text is never consulted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ExecFact {
    pub ran: bool,
    #[serde(default)]
    pub ingests_untrusted: bool,
    #[serde(default)]
    pub reads_private: bool,
    #[serde(default)]
    pub label: String,
}

impl ExecFact {
    pub(crate) fn ran(class: ContentClass, label: impl Into<String>) -> Self {
        Self {
            ran: true,
            ingests_untrusted: class.ingests_untrusted,
            reads_private: class.reads_private,
            label: label.into(),
        }
    }

    /// The tool did not start. Reconstruction must not invent a class.
    pub(crate) fn did_not_run() -> Self {
        Self {
            ran: false,
            ingests_untrusted: false,
            reads_private: false,
            label: String::new(),
        }
    }

    /// Discovery (or another tool) reported trust that is not its own spec.
    pub(crate) fn from_exposure(exposure: &OutputExposure, fallback_label: &str) -> Self {
        let label = if exposure.label.is_empty() {
            fallback_label.to_owned()
        } else {
            exposure.label.clone()
        };
        Self {
            ran: true,
            ingests_untrusted: exposure.ingests_untrusted,
            reads_private: exposure.reads_private,
            label,
        }
    }

    fn class(&self) -> ContentClass {
        ContentClass::declared(self.ingests_untrusted, self.reads_private, false)
    }

    fn apply(&self, taint: &mut ContentTaint) {
        if self.ran {
            taint.note(self.class(), &self.label);
        }
    }
}

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

    /// A label for the card, without changing the bits. Used when a stored
    /// message already carries the bits and we only need the name.
    pub(crate) fn note_label(&mut self, label: &str) {
        self.push_label(label);
    }

    pub(crate) fn sources(&self) -> &[String] {
        &self.sources
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
/// Tool results, CLI executions stored on a model step, and external tool
/// metadata contribute only through [`ExecFact`]s written at the time. Result
/// text and the current tool surface are not inputs.
pub(crate) fn observe_history_step(
    taint: &mut ContentTaint,
    name: &str,
    input: &Value,
    output: Option<&Value>,
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
            // New rows carry an explicit fact. Older rows (from before that
            // field existed) still use the structural summary/final_text keys,
            // never a prefix of the note text.
            if out.get("exposure").is_some() {
                if let Some(fact) = out.get("exposure").and_then(parse_fact) {
                    fact.apply(taint);
                }
                return;
            }
            if out.get("summary").is_some() || out.get("final_text").is_some() {
                taint.note(ContentClass::untrusted_private(), "workflow replay");
            }
        }
        "model" => {
            let Some(out) = output else { return };
            apply_fact_list(taint, out.get("executed"));
            apply_fact_list(taint, out.get("metadata"));
        }
        "tools" => {
            let Some(results) = output.and_then(|o| o["results"].as_array()) else {
                return;
            };
            for result in results {
                if let Some(fact) = result.get("fact").and_then(parse_fact) {
                    fact.apply(taint);
                }
            }
        }
        _ => {}
    }
}

fn parse_fact(value: &Value) -> Option<ExecFact> {
    serde_json::from_value(value.clone()).ok()
}

fn apply_fact_list(taint: &mut ContentTaint, value: Option<&Value>) {
    let Some(list) = value.and_then(Value::as_array) else {
        return;
    };
    for item in list {
        if let Some(fact) = parse_fact(item) {
            fact.apply(taint);
        }
    }
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
///
/// Every field the call actually transmits is in the text. A projection that
/// keeps only the first recipient, or only the query, is not used.
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
        return (server, render_fields(input), "body");
    }
    match name {
        "web_search" => {
            let mut payload = str_field("query");
            // `language` and `limit` are query parameters on at least one
            // configured backend. `backend` selects which host is called.
            for (key, kind) in [("language", "str"), ("limit", "num"), ("backend", "str")] {
                if let Some(extra) = field_text(input, key, kind) {
                    payload.push('\n');
                    payload.push_str(key);
                    payload.push_str(": ");
                    payload.push_str(&extra);
                }
            }
            (
                search_host(input.get("backend").and_then(Value::as_str)),
                payload,
                "query",
            )
        }
        "web_extract" | "web_feed" | "web_sitemap" | "web_metadata" | "browser_open" => {
            let url = str_field("url");
            let host = host_of_url(&url);
            // The URL is the request line, query string included. Other keys
            // on these tools are local (how much of the reply to keep).
            (host, url, "url")
        }
        "email_send" | "email_draft" => {
            let to = all_addrs(input.get("to"));
            let cc = all_addrs(input.get("cc"));
            let bcc = all_addrs(input.get("bcc"));
            let mut everyone = to.clone();
            everyone.extend(cc.iter().cloned());
            everyone.extend(bcc.iter().cloned());
            let host = domains_of(&everyone);
            let mut body = String::new();
            push_addrs(&mut body, "To", &to);
            push_addrs(&mut body, "Cc", &cc);
            push_addrs(&mut body, "Bcc", &bcc);
            let account = str_field("account");
            if !account.is_empty() {
                body.push_str("Account: ");
                body.push_str(&account);
                body.push('\n');
            }
            body.push_str("Subject: ");
            body.push_str(&str_field("subject"));
            body.push_str("\n\n");
            body.push_str(&str_field("body"));
            (host, body, "body")
        }
        "email_search" | "email_read" | "email_mark_read" | "email_list_folders" => {
            let account = str_field("account");
            let host = if account.is_empty() {
                "the mailbox".to_owned()
            } else {
                format!("mailbox {account}")
            };
            (host, render_fields(input), "body")
        }
        "shell_run" => ("the shell".to_owned(), render_fields(input), "body"),
        "script_run" => ("a script".to_owned(), render_fields(input), "body"),
        "browser_fill" | "browser_press" | "browser_select" | "browser_click"
        | "browser_scroll" | "browser_tabs" | "browser_back" => {
            ("the browser".to_owned(), render_fields(input), "body")
        }
        "browser_eval" => ("the browser".to_owned(), str_field("js"), "body"),
        "schedule_create" | "schedule_update" | "schedule_resume" => (
            "this bot's scheduler".to_owned(),
            str_field("prompt"),
            "body",
        ),
        other if other.starts_with("desktop_") => {
            ("the desktop".to_owned(), render_fields(input), "body")
        }
        _ => {
            if let Some(url) = input.get("url").and_then(Value::as_str) {
                (host_of_url(url), render_fields(input), "url")
            } else {
                (name.to_owned(), render_fields(input), "body")
            }
        }
    }
}

/// Every object field, in the order the call carried them.
fn render_fields(input: &Value) -> String {
    let Some(obj) = input.as_object() else {
        return input.to_string();
    };
    if obj.is_empty() {
        return String::new();
    }
    obj.iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn field_text(input: &Value, key: &str, kind: &str) -> Option<String> {
    let value = input.get(key)?;
    if kind == "num" {
        return value.as_i64().map(|n| n.to_string()).or_else(|| {
            value
                .as_u64()
                .map(|n| n.to_string())
                .or_else(|| value.as_f64().map(|n| n.to_string()))
        });
    }
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn all_addrs(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn push_addrs(out: &mut String, label: &str, addrs: &[String]) {
    if addrs.is_empty() {
        return;
    }
    out.push_str(label);
    out.push_str(": ");
    out.push_str(&addrs.join(", "));
    out.push('\n');
}

fn domains_of(addrs: &[String]) -> String {
    let mut domains = Vec::new();
    for addr in addrs {
        let domain = domain_of_addr(addr);
        if !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    if domains.is_empty() {
        "unknown".to_owned()
    } else {
        domains.join(", ")
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
