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

/// A source label longer than this is cut. The model chooses the text
/// (`path`, `url`, `query`), so an uncapped label can fill the reason line.
const MAX_LABEL_CHARS: usize = 120;

/// Exposure bits plus the labels the approval card quotes.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentTaint {
    pub exposure: ContentExposure,
    sources: Vec<String>,
    /// Distinct labels dropped because [`MAX_SOURCES`] kept a newer one.
    earlier: usize,
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
        self.earlier += other.earlier;
        for label in &other.sources {
            self.push_label(label);
        }
    }

    fn push_label(&mut self, label: &str) {
        let label = sanitize_label(label);
        if label.is_empty() {
            return;
        }
        if let Some(i) = self.sources.iter().position(|s| s == &label) {
            let kept = self.sources.remove(i);
            self.sources.push(kept);
            return;
        }
        if self.sources.len() >= MAX_SOURCES {
            self.sources.remove(0);
            self.earlier += 1;
        }
        self.sources.push(label);
    }
}

/// Drop control and invisible characters and cap the length. Done when the
/// label is recorded, so a later card cannot grow a model-chosen sentence.
fn sanitize_label(label: &str) -> String {
    let mut out = String::new();
    for c in label.trim().chars() {
        if c.is_control() || protocol::approval_text::is_hidden(c) {
            continue;
        }
        out.push(c);
        if out.chars().count() >= MAX_LABEL_CHARS {
            break;
        }
    }
    out.trim().to_owned()
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
    // Every field is in the text. A per-tool projection drops a field the
    // next schema change adds, and that field can be what leaves the machine.
    let payload = if matches!(name, "email_send" | "email_draft") {
        email_body(input)
    } else {
        render_transmitted(input)
    };
    let (host, kind) = match name {
        "web_search" => (
            search_host(input.get("backend").and_then(Value::as_str)),
            "query",
        ),
        "web_extract" | "web_feed" | "web_sitemap" | "web_metadata" | "browser_open" => {
            (host_of_url(&str_field("url")), "url")
        }
        "email_send" | "email_draft" => {
            let mut everyone = all_addrs(input.get("to"));
            everyone.extend(all_addrs(input.get("cc")));
            everyone.extend(all_addrs(input.get("bcc")));
            (domains_of(&everyone), "body")
        }
        "email_search" | "email_read" | "email_mark_read" | "email_list_folders" => {
            let account = str_field("account");
            let host = if account.is_empty() {
                "the mailbox".to_owned()
            } else {
                format!("mailbox {account}")
            };
            (host, "body")
        }
        "shell_run" => ("the shell".to_owned(), "body"),
        "script_run" => ("a script".to_owned(), "body"),
        "browser_fill" | "browser_press" | "browser_select" | "browser_click"
        | "browser_scroll" | "browser_tabs" | "browser_back" | "browser_eval" => {
            ("the browser".to_owned(), "body")
        }
        "schedule_create" | "schedule_update" | "schedule_resume" => {
            ("this bot's scheduler".to_owned(), "body")
        }
        other if other.starts_with("desktop_") => ("the desktop".to_owned(), "body"),
        _ => {
            if input.get("url").and_then(Value::as_str).is_some() {
                (host_of_url(&str_field("url")), "url")
            } else {
                (name.to_owned(), "body")
            }
        }
    };
    (host, payload, kind)
}

/// Every object field. A lone `url` or `query` stays the bare string, matching
/// cards that predate the all-fields rule. Any other field is appended, URL
/// or query first, so a new schema property cannot leave unseen.
fn render_transmitted(input: &Value) -> String {
    let Some(obj) = input.as_object() else {
        return display_value(input);
    };
    if obj.is_empty() {
        return String::new();
    }
    if obj.len() == 1 {
        let (key, value) = obj.iter().next().unwrap();
        if let Some(text) = value.as_str()
            && matches!(
                key.as_str(),
                "url" | "query" | "js" | "prompt" | "command" | "body" | "text"
            )
        {
            return text.to_owned();
        }
    }
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort_by_key(|key| match key.as_str() {
        "url" => 0,
        "query" => 1,
        _ => 2,
    });
    let mut lines = Vec::with_capacity(keys.len());
    for key in keys {
        let value = &obj[key];
        if matches!(key.as_str(), "url" | "query")
            && let Some(text) = value.as_str()
        {
            lines.push(text.to_owned());
            continue;
        }
        lines.push(format!("{key}: {}", display_value(value)));
    }
    lines.join("\n")
}

/// Every object field, in the order the call carried them.
fn render_fields(input: &Value) -> String {
    let Some(obj) = input.as_object() else {
        return display_value(input);
    };
    if obj.is_empty() {
        return String::new();
    }
    obj.iter()
        .map(|(k, v)| format!("{k}: {}", display_value(v)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Recipients, subject and body, then any field that layout does not already name.
fn email_body(input: &Value) -> String {
    let to = all_addrs(input.get("to"));
    let cc = all_addrs(input.get("cc"));
    let bcc = all_addrs(input.get("bcc"));
    let mut body = String::new();
    push_addrs(&mut body, "To", &to);
    push_addrs(&mut body, "Cc", &cc);
    push_addrs(&mut body, "Bcc", &bcc);
    let account = input
        .get("account")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !account.is_empty() {
        body.push_str("Account: ");
        body.push_str(account);
        body.push('\n');
    }
    body.push_str("Subject: ");
    body.push_str(
        input
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    body.push_str("\n\n");
    body.push_str(
        input
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if let Some(obj) = input.as_object() {
        for (key, value) in obj {
            if matches!(
                key.as_str(),
                "to" | "cc" | "bcc" | "account" | "subject" | "body"
            ) {
                continue;
            }
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(key);
            body.push_str(": ");
            body.push_str(&display_value(value));
            body.push('\n');
        }
    }
    body
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
        // One string may carry several addresses ("a@evil.test, b@other.test").
        for part in addr.split([',', ';']) {
            let domain = domain_of_addr(part);
            if domain != "unknown" && !domains.contains(&domain) {
                domains.push(domain);
            }
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
    // `url` treats `\` as `/` for http(s). A hand-rolled split on `/` does not,
    // so `https://evil.example\@trusted.example/` used to name trusted.example
    // in the footer while the request went to evil.example. A backslash, or a
    // URL the parser rejects, is not given a host name at all.
    if url.contains('\\') {
        return "unparseable URL".to_owned();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "unparseable URL".to_owned())
}

/// A click names an on-screen target. `prompt_text` is typed into the page,
/// so that call can carry data even though the tool is still called a click.
pub(crate) fn call_can_egress(name: &str, class: ContentClass, input: &Value) -> bool {
    if !class.egress {
        return false;
    }
    if name == "browser_click" {
        return input
            .get("prompt_text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
    }
    true
}

/// Any string [`protocol::approval_text::scrub`] would shorten. The card must
/// not be offered for that input: what the user would see is not what is sent.
pub(crate) fn input_hides_characters(value: &Value) -> bool {
    match value {
        Value::String(text) => protocol::approval_text::hides_characters(text),
        Value::Array(items) => items.iter().any(input_hides_characters),
        Value::Object(map) => map.values().any(input_hides_characters),
        _ => false,
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
    let mut read: Vec<String> = taint.sources.clone();
    if taint.earlier > 0 {
        let word = if taint.earlier == 1 { "read" } else { "reads" };
        read.push(format!("and {} earlier {word}", taint.earlier));
    }
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

#[cfg(test)]
mod card_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use builtin_tools::{
        BuiltinConfig, GateRequest, ScheduleCaller, ScheduleChange, ScheduleHost, SchedulePreview,
        ScheduleRequest, ScheduleView, ToolGate,
    };
    use secret_store::SecretStore;
    use serde_json::{Value, json};
    use tool_registry::{ContentClass, ToolRegistry};

    use super::{ContentTaint, egress_payload};

    #[test]
    fn browser_open_card_contains_every_transmitted_field() {
        let (_host, payload, kind) = egress_payload(
            "browser_open",
            &json!({
                "url": "https://evil.example/",
                "dialog": "accept",
                "prompt_text": "SECRET_DATA",
                "profile": "logged-in"
            }),
        );
        assert_eq!(kind, "url");
        assert!(payload.starts_with("https://evil.example/"), "{payload}");
        assert!(payload.contains("prompt_text: SECRET_DATA"), "{payload}");
        assert!(payload.contains("dialog: accept"), "{payload}");
        assert!(payload.contains("profile: logged-in"), "{payload}");
    }

    #[test]
    fn a_backslash_cannot_rename_the_destination_host() {
        let (host, payload, _) = egress_payload(
            "web_extract",
            &json!({"url": "https://evil.example\\@trusted.example/x"}),
        );
        assert_eq!(host, "unparseable URL");
        assert!(payload.contains("evil.example"), "{payload}");
        assert!(payload.contains("trusted.example"), "{payload}");
        let (host, _, _) =
            egress_payload("browser_open", &json!({"url": "https://evil.example/hook"}));
        assert_eq!(host, "evil.example");
        let (host, _, _) = egress_payload(
            "email_send",
            &json!({
                "to": "a@evil.example, b@trusted.example",
                "subject": "s",
                "body": "b"
            }),
        );
        assert!(host.contains("evil.example"), "{host}");
        assert!(host.contains("trusted.example"), "{host}");
    }

    #[test]
    fn the_card_keeps_the_most_recent_sources_and_bounds_labels() {
        let mut taint = ContentTaint::default();
        for i in 0..8 {
            taint.note(
                ContentClass::untrusted_private(),
                &format!("file junk{i}.txt"),
            );
        }
        taint.note(
            ContentClass::untrusted_private(),
            "file C:/Users/me/.ssh/id_rsa",
        );
        assert_eq!(taint.sources().len(), 8);
        assert_eq!(
            taint.sources().last().map(String::as_str),
            Some("file C:/Users/me/.ssh/id_rsa")
        );
        assert!(taint.sources().iter().all(|s| !s.contains("junk0")));
        assert_eq!(taint.earlier, 1);
        let card = super::egress_details(&taint, "web_search", &json!({"query": "q"})).unwrap();
        let reason = card["reason"].as_str().unwrap();
        assert!(reason.contains("id_rsa"), "{reason}");
        assert!(reason.contains("earlier read"), "{reason}");
        assert!(!reason.contains("junk0"), "{reason}");

        let mut capped = ContentTaint::default();
        capped.note(
            ContentClass::private_read(),
            &format!("file {}", "a".repeat(400)),
        );
        capped.note(
            ContentClass::private_read(),
            "file notes\u{1b}[2Jsafe to approve",
        );
        assert!(capped.sources()[0].chars().count() <= 120);
        assert!(!capped.sources()[1].contains('\u{1b}'));
    }

    struct AllowAll;

    #[async_trait]
    impl ToolGate for AllowAll {
        async fn authorize(&self, _req: &GateRequest) -> Result<(), String> {
            Ok(())
        }
    }

    struct NoSched;

    impl ScheduleHost for NoSched {
        fn list(&self, _: &ScheduleCaller) -> Result<Vec<ScheduleView>, String> {
            Err("unused".into())
        }
        fn preview(
            &self,
            _: &ScheduleCaller,
            _: &ScheduleChange,
        ) -> Result<SchedulePreview, String> {
            Err("unused".into())
        }
        fn discard(&self, _: &ScheduleCaller, _: &ScheduleChange, _: &str) {}
        fn create(&self, _: &ScheduleCaller, _: &ScheduleRequest) -> Result<ScheduleView, String> {
            Err("unused".into())
        }
        fn update(
            &self,
            _: &ScheduleCaller,
            _: &str,
            _: &ScheduleRequest,
        ) -> Result<ScheduleView, String> {
            Err("unused".into())
        }
        fn set_enabled(
            &self,
            _: &ScheduleCaller,
            _: &str,
            _: bool,
        ) -> Result<ScheduleView, String> {
            Err("unused".into())
        }
        fn delete(&self, _: &ScheduleCaller, _: &str) -> Result<ScheduleView, String> {
            Err("unused".into())
        }
    }

    #[test]
    fn every_egress_tool_schema_field_is_on_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new();
        let mut cfg = BuiltinConfig::new(dir.path().to_path_buf(), Arc::new(AllowAll));
        cfg.web = Some(builtin_tools::web_tools::WebToolsConfig::default());
        let store = Arc::new(secret_store::MemoryStore::new());
        store.set("email/test", "password").unwrap();
        let account = builtin_tools::connector_email::AccountConfig {
            name: "test".to_string(),
            address: "user@example.com".to_string(),
            username: String::new(),
            password: secret_store::SecretRef::new("email/test").unwrap(),
            imap: builtin_tools::connector_email::ImapSettings {
                host: "imap.example.com".to_string(),
                port: 993,
            },
            smtp: builtin_tools::connector_email::SmtpSettings {
                host: "smtp.example.com".to_string(),
                port: 465,
                security: builtin_tools::connector_email::SmtpSecurity::Implicit,
            },
            drafts_folder: "Drafts".to_string(),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: None,
            allowed_senders: Vec::new(),
            kind: builtin_tools::connector_email::AccountKind::ImapSmtp,
            inbox_id: None,
        };
        cfg.email = Some(Arc::new(
            builtin_tools::connector_email::EmailConnector::new(vec![account], store).unwrap(),
        ));
        let _ = builtin_tools::register_builtin_tools(&registry, cfg);
        let host: Arc<dyn ScheduleHost> = Arc::new(NoSched);
        for tool in builtin_tools::schedule::tools(&host) {
            registry.register(tool);
        }

        let mut checked = 0usize;
        let mut saw_browser_open = false;
        for spec in registry.list() {
            if !spec.content.egress {
                continue;
            }
            let Some(props) = spec
                .input_schema
                .get("properties")
                .and_then(Value::as_object)
            else {
                continue;
            };
            if props.is_empty() {
                continue;
            }
            let input = Value::Object(
                props
                    .keys()
                    .map(|key| (key.clone(), Value::String(format!("SNTNL_{key}_END"))))
                    .collect(),
            );
            let (_host, payload, _) = egress_payload(&spec.name, &input);
            for key in props.keys() {
                let sentinel = format!("SNTNL_{key}_END");
                assert!(
                    payload.contains(&sentinel),
                    "{} hides `{key}`: {payload}",
                    spec.name
                );
            }
            if spec.name == "browser_open" {
                saw_browser_open = true;
            }
            checked += 1;
        }
        assert!(saw_browser_open, "browser_open was not in the audited set");
        assert!(checked > 8, "only {checked} egress tools were audited");
    }
}
