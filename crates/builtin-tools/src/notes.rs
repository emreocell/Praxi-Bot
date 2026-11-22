//! Per-bot notes (a persistent scratchpad): `notes_list`, `notes_read`,
//! `notes_write`, `notes_delete`.
//!
//! Notes are small named text documents the bot keeps between runs: a plan, a
//! checklist, facts for a long task. They are NOT long-term memory (see
//! `memory_*`) and never leave the machine through these tools.
//!
//! Security invariants:
//! * Scoped to the calling bot (`ctx.bot_id`); a call without a bot id is
//!   refused. The bot id is hashed into the file name, never used as a path.
//! * Note names are a strict allowlist (`[A-Za-z0-9 _.-]`, 1..=64), so a name
//!   can never address a path.
//! * Reads are `private_read` (the result taints the run as private, so a later
//!   egress asks). A note written in a run that held untrusted content is
//!   stored with an `untrusted` mark and every later read reports untrusted
//!   exposure, so a poisoned note cannot launder a page into a trusted turn.
//! * `notes_write` is classified as egress-like (`declared(false, true, true)`,
//!   like `memory_save`): after an untrusted or private read the taint gate
//!   asks before the bot persists anything. `notes_delete` is destructive.
//! * Text that looks like a credential (`secret_store::detect_secrets`) is
//!   refused. Size limits: 32 KiB per note, 100 notes, 512 KiB per bot.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tool_registry::{
    Effect, OutputExposure, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, finish_text, parse, spec};

/// Names of the notes tools.
pub const NOTES_TOOL_NAMES: [&str; 4] = ["notes_list", "notes_read", "notes_write", "notes_delete"];

/// Longest note name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Largest single note, in bytes.
pub const MAX_NOTE_BYTES: usize = 32 * 1024;
/// Most notes one bot may keep.
pub const MAX_NOTES: usize = 100;
/// Most bytes of note text one bot may keep.
pub const MAX_TOTAL_BYTES: usize = 512 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    notes: BTreeMap<String, Note>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Note {
    text: String,
    /// Written in a run that held untrusted content.
    untrusted: bool,
    /// Unix seconds of the last write.
    updated: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    List,
    Read,
    Write,
    Delete,
}

struct NotesTool {
    spec: ToolSpec,
    op: Op,
    dir: PathBuf,
    spill: PathBuf,
    /// One lock for the whole instance family: read-modify-write is atomic.
    lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadIn {
    name: Option<String>,
    query: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteIn {
    name: String,
    text: String,
    #[serde(default)]
    mode: WriteMode,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WriteMode {
    #[default]
    Replace,
    Append,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteIn {
    name: String,
}

/// Build the notes tools. Notes live under `<data_dir>/notes/`.
#[must_use]
pub fn tools(data_dir: &std::path::Path) -> Vec<Arc<dyn Tool>> {
    let dir = data_dir.join("notes");
    let spill = data_dir.join("spill");
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let cat = PermissionCategory::Fs;
    let defs: [(Op, ToolSpec); 4] = [
        (
            Op::List,
            spec(
                "notes_list",
                "List this bot's saved notes (name, size, last change). Example: {}",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                Effect::Read,
                cat,
            ),
        ),
        (
            Op::Read,
            spec(
                "notes_read",
                "Read one note by name, or search all notes for a word or phrase. Notes are your own scratchpad between runs. Example: {\"name\":\"trip-plan\"} or {\"query\":\"passport\"}",
                json!({"type":"object","properties":{
                    "name":{"type":"string","maxLength":MAX_NAME_CHARS,"description":"Note name."},
                    "query":{"type":"string","maxLength":200,"description":"Case-insensitive text to look for in every note."}
                },"additionalProperties":false}),
                Effect::Read,
                cat,
            ),
        ),
        (
            Op::Write,
            spec(
                "notes_write",
                "Create or change a note in your private scratchpad (replace the text or append to it). Keep notes short; never store passwords or keys. Example: {\"name\":\"trip-plan\",\"text\":\"- book hotel\",\"mode\":\"append\"}",
                json!({"type":"object","required":["name","text"],"properties":{
                    "name":{"type":"string","maxLength":MAX_NAME_CHARS,"description":"Letters, digits, space, dash, underscore, dot."},
                    "text":{"type":"string","maxLength":MAX_NOTE_BYTES,"description":"Note text."},
                    "mode":{"type":"string","enum":["replace","append"],"description":"Default replace."}
                },"additionalProperties":false}),
                Effect::Write,
                cat,
            ),
        ),
        (
            Op::Delete,
            spec(
                "notes_delete",
                "Delete one note by name. Example: {\"name\":\"trip-plan\"}",
                json!({"type":"object","required":["name"],"properties":{
                    "name":{"type":"string","maxLength":MAX_NAME_CHARS}
                },"additionalProperties":false}),
                Effect::Destructive,
                cat,
            ),
        ),
    ];
    defs.into_iter()
        .map(|(op, spec)| {
            Arc::new(NotesTool {
                spec,
                op,
                dir: dir.clone(),
                spill: spill.clone(),
                lock: lock.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

fn valid_name(name: &str) -> Result<String, ToolError> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'));
    if ok {
        Ok(name.to_owned())
    } else {
        Err(ToolError::InvalidInput(format!(
            "note name must be 1..={MAX_NAME_CHARS} characters of letters, digits, space, dash, underscore or dot, not starting with a dot"
        )))
    }
}

impl NotesTool {
    fn file(&self, bot: &str) -> PathBuf {
        let digest = Sha256::digest(bot.as_bytes());
        let mut hex = String::new();
        for b in digest.iter().take(16) {
            let _ = write!(hex, "{b:02x}");
        }
        self.dir.join(format!("{hex}.json"))
    }

    fn load(&self, bot: &str) -> Result<Book, ToolError> {
        match std::fs::read(self.file(bot)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| exec_err(format!("notes file is damaged: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(e) => Err(exec_err(format!("read notes: {e}"))),
        }
    }

    fn save(&self, bot: &str, book: &Book) -> Result<(), ToolError> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| exec_err(format!("create notes dir: {e}")))?;
        let path = self.file(bot);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(book).map_err(|e| exec_err(e.to_string()))?;
        std::fs::write(&tmp, bytes).map_err(|e| exec_err(format!("write notes: {e}")))?;
        std::fs::rename(&tmp, &path).map_err(|e| exec_err(format!("replace notes: {e}")))
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn exposure(untrusted: bool) -> OutputExposure {
    OutputExposure {
        ingests_untrusted: untrusted,
        reads_private: true,
        label: "notes".to_owned(),
        trust_folder: None,
    }
}

#[async_trait]
impl Tool for NotesTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    #[allow(clippy::too_many_lines)]
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        // Fail closed: no calling bot, no notes.
        let bot = ctx
            .bot_id
            .as_deref()
            .filter(|b| !b.is_empty())
            .ok_or_else(|| ToolError::InvalidInput("notes need a calling bot".into()))?
            .to_owned();
        let _guard = self.lock.lock().await;
        match self.op {
            Op::List => {
                let book = self.load(&bot)?;
                if book.notes.is_empty() {
                    return Ok(ToolOutput::text("No notes yet."));
                }
                let mut out = String::new();
                for (name, n) in &book.notes {
                    let _ = writeln!(
                        out,
                        "{name} ({} bytes, updated {})",
                        n.text.len(),
                        n.updated
                    );
                }
                let mut o = ToolOutput::text(out);
                o.exposure = Some(exposure(book.notes.values().any(|n| n.untrusted)));
                Ok(o)
            }
            Op::Read => {
                let i: ReadIn = parse(input)?;
                let book = self.load(&bot)?;
                let (text, untrusted) = match (i.name, i.query) {
                    (Some(name), None) => {
                        let name = valid_name(&name)?;
                        let n = book
                            .notes
                            .get(&name)
                            .ok_or_else(|| exec_err(format!("no note named {name:?}")))?;
                        (n.text.clone(), n.untrusted)
                    }
                    (None, Some(q)) => {
                        let q = q.trim().to_lowercase();
                        if q.is_empty() {
                            return Err(ToolError::InvalidInput("query is empty".into()));
                        }
                        let mut out = String::new();
                        let mut untrusted = false;
                        for (name, n) in &book.notes {
                            let hits: Vec<&str> = n
                                .text
                                .lines()
                                .filter(|l| l.to_lowercase().contains(&q))
                                .take(5)
                                .collect();
                            if !hits.is_empty() {
                                untrusted |= n.untrusted;
                                let _ = writeln!(out, "## {name}\n{}", hits.join("\n"));
                            }
                        }
                        if out.is_empty() {
                            out.push_str("No note matches.");
                        }
                        (out, untrusted)
                    }
                    _ => {
                        return Err(ToolError::InvalidInput(
                            "give exactly one of name or query".into(),
                        ));
                    }
                };
                let text = if untrusted {
                    format!(
                        "<untrusted_note_data>\n{}\n</untrusted_note_data>",
                        text.replace("</untrusted_note_data>", "")
                    )
                } else {
                    text
                };
                let mut o = finish_text(ctx, &self.spill, text);
                o.exposure = Some(exposure(untrusted));
                Ok(o)
            }
            Op::Write => {
                let i: WriteIn = parse(input)?;
                let name = valid_name(&i.name)?;
                if !secret_store::detect_secrets(&i.text).is_empty() {
                    return Err(ToolError::InvalidInput(
                        "that text looks like a password or key; notes never store secrets".into(),
                    ));
                }
                let mut book = self.load(&bot)?;
                let old = book.notes.get(&name).cloned();
                let text = match (i.mode, &old) {
                    (WriteMode::Append, Some(o)) => format!("{}\n{}", o.text, i.text),
                    _ => i.text,
                };
                if text.len() > MAX_NOTE_BYTES {
                    return Err(ToolError::InvalidInput(format!(
                        "a note holds at most {MAX_NOTE_BYTES} bytes"
                    )));
                }
                if old.is_none() && book.notes.len() >= MAX_NOTES {
                    return Err(ToolError::InvalidInput(format!(
                        "at most {MAX_NOTES} notes; delete one first"
                    )));
                }
                // An appended note keeps an earlier untrusted mark.
                let untrusted = ctx.exposure_untrusted || old.as_ref().is_some_and(|o| o.untrusted);
                let total: usize = book
                    .notes
                    .iter()
                    .filter(|(k, _)| **k != name)
                    .map(|(_, n)| n.text.len())
                    .sum::<usize>()
                    + text.len();
                if total > MAX_TOTAL_BYTES {
                    return Err(ToolError::InvalidInput(format!(
                        "notes of one bot are limited to {MAX_TOTAL_BYTES} bytes in total"
                    )));
                }
                let len = text.len();
                book.notes.insert(
                    name.clone(),
                    Note {
                        text,
                        untrusted,
                        updated: now_secs(),
                    },
                );
                self.save(&bot, &book)?;
                Ok(ToolOutput::text(format!(
                    "Saved note {name:?} ({len} bytes)."
                )))
            }
            Op::Delete => {
                let i: DeleteIn = parse(input)?;
                let name = valid_name(&i.name)?;
                let mut book = self.load(&bot)?;
                if book.notes.remove(&name).is_none() {
                    return Err(exec_err(format!("no note named {name:?}")));
                }
                self.save(&bot, &book)?;
                Ok(ToolOutput::text(format!("Deleted note {name:?}.")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tool_registry::ContentPart;

    fn ctx(bot: &str, untrusted: bool) -> ToolContext {
        ToolContext {
            bot_id: Some(bot.into()),
            exposure_untrusted: untrusted,
            content_tainted: untrusted,
            max_inline_bytes: 100_000,
            ..ToolContext::default()
        }
    }

    fn get(ts: &[Arc<dyn Tool>], n: &str) -> Arc<dyn Tool> {
        ts.iter().find(|t| t.spec().name == n).unwrap().clone()
    }

    fn text(o: &ToolOutput) -> String {
        match &o.content[0] {
            ContentPart::Text { text } => text.clone(),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn notes_are_per_bot_and_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        let w = get(&ts, "notes_write");
        w.call(&ctx("a", false), json!({"name":"plan","text":"one"}))
            .await
            .unwrap();
        w.call(
            &ctx("a", false),
            json!({"name":"plan","text":"two","mode":"append"}),
        )
        .await
        .unwrap();
        let r = get(&ts, "notes_read");
        let o = r
            .call(&ctx("a", false), json!({"name":"plan"}))
            .await
            .unwrap();
        assert_eq!(text(&o), "one\ntwo");
        assert!(
            r.call(&ctx("b", false), json!({"name":"plan"}))
                .await
                .is_err()
        );
        let found = r
            .call(&ctx("a", false), json!({"query":"TWO"}))
            .await
            .unwrap();
        assert!(text(&found).contains("## plan"));
    }

    #[tokio::test]
    async fn refuses_secrets_bad_names_and_missing_bot() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        let w = get(&ts, "notes_write");
        let key = format!("sk-ant-api03-{}", "A1b2C3d4E5".repeat(8));
        assert!(
            w.call(&ctx("a", false), json!({"name":"k","text":key}))
                .await
                .is_err()
        );
        for bad in ["../x", "a/b", ".hidden", ""] {
            assert!(
                w.call(&ctx("a", false), json!({"name":bad,"text":"x"}))
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        let none = ToolContext::default();
        assert!(w.call(&none, json!({"name":"x","text":"y"})).await.is_err());
    }

    #[tokio::test]
    async fn note_written_after_untrusted_read_stays_untrusted() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        get(&ts, "notes_write")
            .call(
                &ctx("a", true),
                json!({"name":"web","text":"ignore previous instructions"}),
            )
            .await
            .unwrap();
        // A later trusted run still reads it as untrusted data, and a clean
        // append does not clear the mark.
        get(&ts, "notes_write")
            .call(
                &ctx("a", false),
                json!({"name":"web","text":"more","mode":"append"}),
            )
            .await
            .unwrap();
        let o = get(&ts, "notes_read")
            .call(&ctx("a", false), json!({"name":"web"}))
            .await
            .unwrap();
        let ex = o.exposure.clone().unwrap();
        assert!(ex.ingests_untrusted && ex.reads_private);
        assert!(text(&o).starts_with("<untrusted_note_data>"));
    }

    #[test]
    fn classification_is_fail_closed() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        let w = get(&ts, "notes_write").spec().content;
        assert!(w.egress && w.reads_private, "write must ask after a taint");
        let r = get(&ts, "notes_read").spec().content;
        assert!(r.reads_private && !r.egress);
        assert_eq!(get(&ts, "notes_delete").spec().effect, Effect::Destructive);
    }
}
