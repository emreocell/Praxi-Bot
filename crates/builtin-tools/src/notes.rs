//! Per-bot notes (a persistent scratchpad): `notes_read` (list, read one,
//! or search) and `notes_write` (replace, append or delete).
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
//!   asks before the bot persists or deletes anything. Only two tools exist
//!   because the direct tool budget is nearly full (see `docs/design/agent-tools.md`).
//! * Text that looks like a credential (`secret_store::detect_secrets`) is
//!   refused, also when it hides in a fenced block (backtick or tilde fences of
//!   any length), is split over lines, or only forms a key once appended to the
//!   existing note. Writes to one note are serialised by a per-note lock. Size limits: 32 KiB per note, 100 notes, 512 KiB per bot.

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
pub const NOTES_TOOL_NAMES: [&str; 2] = ["notes_read", "notes_write"];

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
    Read,
    Write,
}

struct NotesTool {
    spec: ToolSpec,
    op: Op,
    dir: PathBuf,
    spill: PathBuf,
    /// Per-note locks (shared by the instance family): two writers of the same
    /// note never interleave their read-modify-write.
    locks: Arc<NoteLocks>,
}

type NoteGate = Arc<tokio::sync::Mutex<()>>;

/// One async lock per (bot file, note name), plus a short sync lock around
/// the book file itself (notes of one bot share a file).
#[derive(Default)]
struct NoteLocks {
    per_note: std::sync::Mutex<BTreeMap<(String, String), NoteGate>>,
    file: std::sync::Mutex<()>,
}

impl NoteLocks {
    fn note(&self, bot: &str, name: &str) -> NoteGate {
        let mut map = self
            .per_note
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry((bot.to_owned(), name.to_owned()))
            .or_default()
            .clone()
    }
}

/// Does `text` hold something that looks like a credential, even when it is
/// wrapped in a code fence or broken over lines?
fn looks_secret(text: &str) -> bool {
    if !secret_store::detect_secrets(text).is_empty() {
        return true;
    }
    // Drop fence lines (``` or ~~~, any length, optional info string) and join
    // the remaining lines, so a key split by a line break is whole again.
    let mut joined = String::with_capacity(text.len());
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            continue;
        }
        joined.push_str(t.trim_end_matches('\\'));
    }
    joined != text && !secret_store::detect_secrets(&joined).is_empty()
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
    /// Remove the note (`text` must be empty).
    Delete,
}

/// Build the notes tools. Notes live under `<data_dir>/notes/`.
#[must_use]
pub fn tools(data_dir: &std::path::Path) -> Vec<Arc<dyn Tool>> {
    let dir = data_dir.join("notes");
    let spill = data_dir.join("spill");
    let locks = Arc::new(NoteLocks::default());
    let cat = PermissionCategory::Fs;
    let defs: [(Op, ToolSpec); 2] = [
        (
            Op::Read,
            spec(
                "notes_read",
                "Read your notes: with no arguments list them, with a name read one note, with a query search all notes. Notes are your own scratchpad between runs. Example: {} or {\"name\":\"trip-plan\"} or {\"query\":\"passport\"}",
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
                "Create, change or delete a note in your private scratchpad (mode replace, append or delete; delete needs empty text). Keep notes short; never store passwords or keys. Example: {\"name\":\"trip-plan\",\"text\":\"- book hotel\",\"mode\":\"append\"}",
                json!({"type":"object","required":["name","text"],"properties":{
                    "name":{"type":"string","maxLength":MAX_NAME_CHARS,"description":"Letters, digits, space, dash, underscore, dot."},
                    "text":{"type":"string","maxLength":MAX_NOTE_BYTES,"description":"Note text (empty for delete)."},
                    "mode":{"type":"string","enum":["replace","append","delete"],"description":"Default replace."}
                },"additionalProperties":false}),
                Effect::Write,
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
                locks: locks.clone(),
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
        match self.op {
            Op::Read => {
                let i: ReadIn = parse(input)?;
                let book = {
                    let _f = self
                        .locks
                        .file
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    self.load(&bot)?
                };
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
                    (None, None) => {
                        let mut out = String::new();
                        for (name, n) in &book.notes {
                            let _ = writeln!(
                                out,
                                "{name} ({} bytes, updated {})",
                                n.text.len(),
                                n.updated
                            );
                        }
                        if out.is_empty() {
                            out.push_str("No notes yet.");
                        }
                        (out, book.notes.values().any(|n| n.untrusted))
                    }
                    (Some(_), Some(_)) => {
                        return Err(ToolError::InvalidInput(
                            "give a name or a query, not both".into(),
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
                let note_lock = self.locks.note(&bot, &name);
                let _note = note_lock.lock().await;
                let _file = self
                    .locks
                    .file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut book = self.load(&bot)?;
                let old = book.notes.get(&name).cloned();
                if i.mode == WriteMode::Delete {
                    if !i.text.is_empty() {
                        return Err(ToolError::InvalidInput("delete takes an empty text".into()));
                    }
                    if book.notes.remove(&name).is_none() {
                        return Err(exec_err(format!("no note named {name:?}")));
                    }
                    self.save(&bot, &book)?;
                    return Ok(ToolOutput::text(format!("Deleted note {name:?}.")));
                }
                let text = match (i.mode, &old) {
                    (WriteMode::Append, Some(o)) => format!("{}\n{}", o.text, i.text),
                    _ => i.text,
                };
                if looks_secret(&text) {
                    return Err(ToolError::InvalidInput(
                        "that text looks like a password or key; notes never store secrets".into(),
                    ));
                }
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

    #[tokio::test]
    async fn refuses_fenced_split_and_appended_secrets() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        let w = get(&ts, "notes_write");
        let key = format!("sk-ant-api03-{}", "A1b2C3d4E5".repeat(8));
        let (first, second) = key.split_at(18);
        assert!(
            secret_store::detect_secrets(first).is_empty()
                && secret_store::detect_secrets(second).is_empty()
        );
        let cases = [
            format!(
                "~~~~~text
{key}
~~~~~"
            ),
            format!(
                "`````
{key}
`````"
            ),
            format!(
                "{first}
{second}"
            ),
            format!(
                "```
{first}
```
```
{second}
```"
            ),
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(
                w.call(&ctx("a", false), json!({"name":"k","text":c}))
                    .await
                    .is_err(),
                "case {i}"
            );
        }
        // Two harmless appends that together form a key are refused.
        w.call(&ctx("a", false), json!({"name":"s","text":first}))
            .await
            .unwrap();
        assert!(
            w.call(
                &ctx("a", false),
                json!({"name":"s","text":second,"mode":"append"})
            )
            .await
            .is_err()
        );
        // Normal notes with fences and several lines still work.
        w.call(
            &ctx("a", false),
            json!({"name":"n","text":"```rust
let x = 1;
```
- milk
- eggs"}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn concurrent_appends_to_one_note_are_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let ts = tools(dir.path());
        let w = get(&ts, "notes_write");
        let mut set = tokio::task::JoinSet::new();
        for i in 0..24 {
            let w = w.clone();
            let name = if i % 2 == 0 { "one" } else { "two" };
            set.spawn(async move {
                w.call(
                    &ctx("a", false),
                    json!({"name":name,"text":format!("line{i}"),"mode":"append"}),
                )
                .await
                .unwrap();
            });
        }
        while set.join_next().await.is_some() {}
        let reader = get(&ts, "notes_read");
        for (name, par) in [("one", 0), ("two", 1)] {
            let o = reader
                .call(&ctx("a", false), json!({"name":name}))
                .await
                .unwrap();
            let body = text(&o);
            for i in (0..24).filter(|i| i % 2 == par) {
                assert!(body.contains(&format!("line{i}")), "{name} lost line{i}");
            }
        }
    }

    #[test]
    fn classification_is_fail_closed() {
        let d = tempfile::tempdir().unwrap();
        let ts = tools(d.path());
        let w = get(&ts, "notes_write").spec().content;
        assert!(w.egress && w.reads_private, "write must ask after a taint");
        let r = get(&ts, "notes_read").spec().content;
        assert!(r.reads_private && !r.egress);
    }
}
