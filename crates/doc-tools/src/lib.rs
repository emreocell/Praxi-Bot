//! Document and data tools for the agent: PDF, DOCX, XLSX, CSV and JSON.
//!
//! Tools (all permission category `Fs`):
//!
//! | tool            | effect     | what it does                                       |
//! |-----------------|------------|----------------------------------------------------|
//! | `pdf_extract`   | read       | text per page, best-effort tables                  |
//! | `docx_read`     | read       | headings, paragraphs and tables of a `.docx`       |
//! | `docx_write`    | write      | create a `.docx` from headings/paragraphs/tables   |
//! | `xlsx_read`     | read       | sheets, ranges and typed cells (`.xlsx/.xlsb/.xls`) |
//! | `xlsx_write`    | write      | create an `.xlsx` with typed cells and formulas    |
//! | `xlsx_append_rows`  | write  | add rows to an existing `.xlsx`, rest kept as is   |
//! | `xlsx_update_cells` | write  | set cells of an existing `.xlsx`, rest kept as is  |
//! | `docx_append`   | write      | add blocks to the end of an existing `.docx`       |
//! | `pptx_read`     | read       | text of each slide of a `.pptx`                    |
//! | `csv_read`      | read       | paged CSV/TSV reading                              |
//! | `csv_transform` | read/write | filter / select / sort / group / limit pipeline    |
//! | `json_query`    | read       | JSONPath (RFC 9535) query over a file or inline    |
//! | `doc_convert`   | write      | Office <-> PDF through LibreOffice (see `convert`)  |
//! | `doc_preview`   | write      | PNG page previews (pdftoppm / LibreOffice)         |
//!
//! Every tool returns two content parts: a compact markdown preview for the
//! model (`ContentPart::Text`) and the structured result (`ContentPart::Json`).
//!
//! # Permissions
//!
//! File paths are always passed in the `path` (and, for `csv_transform`, the
//! optional `output_path`) input fields, exactly like the `fs_*` tools of
//! `builtin-tools`. The permission engine gates them by category and effect
//! before `call`. `write_file` also refuses the daemon data directory and
//! native CLI configuration, so a caller that skips that gate still cannot
//! plant a skill or a workflow there. Write tools never overwrite an existing
//! file unless `overwrite: true` is given. Inputs larger than
//! [`DocToolsConfig::max_input_bytes`] are rejected.
//!
//! # `doc_convert` / `doc_preview`
//!
//! These two tools run LibreOffice and `pdftoppm` on untrusted documents and fail closed
//! unless the host sets [`ConvertConfig::output_root`] (the bot workspace) and
//! [`ConvertConfig::read_guard`] (the `fs_read` path rule). `DocToolsConfig::default()`
//! leaves both unset, so the tools refuse every call until the host sets them; the daemon
//! does so per call in `builtin-tools`.
//!
//! Network isolation is mandatory too: on Linux the converters run in a network-less
//! sandbox (bubblewrap or `unshare`) and the tools fail closed when none works, unless the
//! owner sets [`ConvertConfig::allow_unsandboxed_convert`] (UNSAFE for untrusted
//! documents). On Windows conversion is off unless
//! [`ConvertConfig::convert_enabled_on_windows`] is set (NTLM / SMB leak risk). Before
//! wiring, run `scripts/check_soffice_no_network.py` against a real LibreOffice.
//!
//! # Wiring
//!
//! ```ignore
//! doc_tools::register(&registry, &doc_tools::DocToolsConfig::default());
//! ```

// Flat declarative tool tables and dispatchers; short names (r, c, i) are idiomatic here.
#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::doc_markdown
)]

mod common;
mod convert;
mod convert_sandbox;
#[cfg(test)]
mod convert_tests;
mod convert_vet;
mod csv_tools;
mod docx;
mod docx_edit;
mod json_query;
mod ooxml;
mod pdf;
mod pdf_budget;
mod pdf_cmap;
mod pptx;
mod xlsx;
mod xlsx_edit;

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};

pub use common::Reply;
pub use convert::{ConvertConfig, default_convert_output, default_preview_dir};

/// Extract an already-authorized PDF or DOCX snapshot without reopening a path.
/// The caller owns path authorization and supplies a display label, never an
/// instruction. Both input and rendered output are bounded by `cfg`.
///
/// # Errors
/// Rejects unsupported formats, oversized input and malformed documents.
pub fn extract_bytes(
    cfg: &DocToolsConfig,
    extension: &str,
    label: &str,
    bytes: &[u8],
) -> Result<Reply, ToolError> {
    if bytes.len() as u64 > cfg.max_input_bytes {
        return Err(ToolError::InvalidInput(
            "document exceeds input limit".into(),
        ));
    }
    let mut reply = std::panic::catch_unwind(|| match extension {
        "pdf" => pdf::from_bytes(cfg, label, bytes),
        "docx" => docx::read_bytes(cfg, label, bytes),
        _ => Err(ToolError::InvalidInput(
            "unsupported document format".into(),
        )),
    })
    .map_err(|_| ToolError::Execution("document parser failed; source was not indexed".into()))??;
    if reply.data["truncated"].as_bool() == Some(true) {
        return Err(ToolError::InvalidInput(
            "document source exceeds extraction limits; no truncated snapshot was indexed".into(),
        ));
    }
    // Tool previews deliberately shorten tables/cells and prose. A knowledge
    // snapshot instead renders all retained structured text or rejects it.
    let complete = if extension == "pdf" {
        let mut text = format!("# {label}\n");
        for page in reply.data["pages"]
            .as_array()
            .ok_or_else(|| ToolError::Execution("missing PDF extraction pages".into()))?
        {
            if page.get("error").is_some() {
                return Err(ToolError::Execution(
                    "PDF source text extraction failed".into(),
                ));
            }
            let _ = write!(
                text,
                "\n## Page {}\n\n{}\n",
                page["page"],
                page["text"].as_str().unwrap_or_default()
            );
        }
        text
    } else {
        complete_docx_text(&reply.data)?
    };
    if complete.len() > cfg.max_output_bytes || complete.chars().count() > cfg.max_preview_chars {
        return Err(ToolError::InvalidInput(
            "document source exceeds extracted text limit; no truncated snapshot was indexed"
                .into(),
        ));
    }
    reply.markdown = complete;
    Ok(reply)
}

/// Which Office format a zip file is (`docx`, `xlsx` or `pptx`), judged by its entries
/// rather than any file name; `None` for other archives.
#[must_use]
pub fn office_extension(bytes: &[u8]) -> Option<&'static str> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    [
        ("word/document.xml", "docx"),
        ("xl/workbook.xml", "xlsx"),
        ("ppt/presentation.xml", "pptx"),
    ]
    .into_iter()
    .find(|(entry, _)| zip.by_name(entry).is_ok())
    .map(|(_, ext)| ext)
}

/// Plain text of a user-attached document (`pdf`, `docx`, `xlsx`, `pptx`), at most
/// `max_bytes` long (cut on a character boundary, with a marker). Bounded like the
/// document tools; malformed or oversized files are an error, never a panic.
///
/// # Errors
/// Unsupported extension, oversized input or a document that cannot be parsed.
pub fn attachment_text(
    cfg: &DocToolsConfig,
    extension: &str,
    label: &str,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<String, ToolError> {
    if bytes.len() as u64 > cfg.max_input_bytes {
        return Err(ToolError::InvalidInput(
            "document exceeds input limit".into(),
        ));
    }
    if matches!(extension, "docx" | "xlsx" | "pptx") {
        check_zip_limits(bytes)?;
    }
    // The parsers are bounded by their own budgets; the deadline is the backstop so one
    // pathological file cannot hold an upload request open. A parser cannot be interrupted
    // from outside, so a timed-out worker is detached and runs on until it ends by itself;
    // it keeps its slot in `EXTRACTIONS` until then, so detached workers stay bounded.
    // Wait for a free slot, but no longer than one extraction may run: a busy or stuck
    // pool is then a clean error, not an unbounded pile of threads.
    let wait_until = std::time::Instant::now() + ATTACHMENT_EXTRACT_TIMEOUT;
    let slot = loop {
        if let Some(slot) = EXTRACTIONS.try_acquire() {
            break slot;
        }
        if std::time::Instant::now() >= wait_until {
            return Err(ToolError::Execution(
                "too many documents are being read right now; try again in a moment".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let (cfg, extension, label, bytes) = (
        cfg.clone(),
        extension.to_owned(),
        label.to_owned(),
        bytes.to_vec(),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _slot = slot;
        let _ = tx.send(attachment_text_inner(
            &cfg, &extension, &label, &bytes, max_bytes,
        ));
    });
    rx.recv_timeout(ATTACHMENT_EXTRACT_TIMEOUT)
        .unwrap_or_else(|_| {
            Err(ToolError::Execution(
                "reading the document took too long".into(),
            ))
        })
}

/// Most attachment extractions in flight at once, timed-out ones included.
const MAX_CONCURRENT_EXTRACTIONS: usize = 2;
static EXTRACTIONS: ExtractionLimiter = ExtractionLimiter::new(MAX_CONCURRENT_EXTRACTIONS);

/// Counting limiter: a slot is held until its guard drops (fail fast, no queueing).
struct ExtractionLimiter {
    max: usize,
    in_flight: std::sync::atomic::AtomicUsize,
}

struct ExtractionSlot(&'static ExtractionLimiter);

impl ExtractionLimiter {
    const fn new(max: usize) -> Self {
        Self {
            max,
            in_flight: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn try_acquire(&'static self) -> Option<ExtractionSlot> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut n = self.in_flight.load(SeqCst);
        loop {
            if n >= self.max {
                return None;
            }
            match self.in_flight.compare_exchange(n, n + 1, SeqCst, SeqCst) {
                Ok(_) => return Some(ExtractionSlot(self)),
                Err(now) => n = now,
            }
        }
    }
}

impl Drop for ExtractionSlot {
    fn drop(&mut self) {
        self.0
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Longest an attachment extraction may run.
const ATTACHMENT_EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Most entries an Office archive may hold.
const MAX_ZIP_ENTRIES: usize = 5_000;
/// Most bytes all entries of an Office archive may expand to.
const MAX_ZIP_UNCOMPRESSED: u64 = 128 * 1024 * 1024;
/// Most bytes one entry may expand to.
const MAX_ZIP_ENTRY: u64 = 64 * 1024 * 1024;

/// Refuse an Office archive that is built to expand: too many entries, or entries that
/// really decompress past the limits. Declared sizes are only a quick first check (a forged
/// header can lie); the actual decompressed bytes are counted with a read limit, and a
/// corrupt entry is refused too.
fn check_zip_limits(bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Read as _;
    let bad = |m: &str| ToolError::InvalidInput(m.into());
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| bad("not a valid Office file"))?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(bad("the document has too many parts"));
    }
    let too_big = || bad("the document expands beyond the size limit");
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|_| bad("not a valid Office file"))?;
        if entry.size() > MAX_ZIP_ENTRY {
            return Err(too_big());
        }
        let allowed = MAX_ZIP_ENTRY.min(MAX_ZIP_UNCOMPRESSED.saturating_sub(total));
        let read = std::io::copy(&mut entry.take(allowed + 1), &mut std::io::sink())
            .map_err(|_| bad("not a valid Office file"))?;
        if read > allowed {
            return Err(too_big());
        }
        total += read;
    }
    Ok(())
}

fn attachment_text_inner(
    cfg: &DocToolsConfig,
    extension: &str,
    label: &str,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<String, ToolError> {
    let mut text = match extension {
        "pdf" | "docx" => {
            let mut big = cfg.clone();
            big.max_preview_chars = max_bytes.saturating_mul(4);
            extract_bytes(&big, extension, label, bytes).map(|r| r.markdown)?
        }
        "xlsx" | "pptx" => std::panic::catch_unwind(|| match extension {
            "xlsx" => xlsx::plain_text(bytes.to_vec(), max_bytes),
            _ => pptx::plain_text(bytes, max_bytes),
        })
        .map_err(|_| ToolError::Execution("document parser failed".into()))??,
        _ => {
            return Err(ToolError::InvalidInput(
                "unsupported document format".into(),
            ));
        }
    };
    if text.len() > max_bytes {
        let mut cut = max_bytes;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[truncated]");
    }
    Ok(text)
}

fn complete_docx_text(data: &Value) -> Result<String, ToolError> {
    use std::fmt::Write as _;
    let mut text = String::new();
    for block in data["blocks"]
        .as_array()
        .ok_or_else(|| ToolError::Execution("missing DOCX extraction blocks".into()))?
    {
        match block["type"].as_str() {
            Some("heading") => {
                let level = usize::try_from(block["level"].as_u64().unwrap_or(1))
                    .unwrap_or(1)
                    .clamp(1, 9);
                let _ = writeln!(
                    text,
                    "{} {}\n",
                    "#".repeat(level),
                    block["text"].as_str().unwrap_or_default()
                );
            }
            Some("paragraph") => {
                let _ = writeln!(text, "{}\n", block["text"].as_str().unwrap_or_default());
            }
            Some("table") => {
                let rows: Vec<Vec<String>> = serde_json::from_value(block["rows"].clone())
                    .map_err(|_| ToolError::Execution("invalid extracted DOCX table".into()))?;
                for (index, row) in rows.iter().enumerate() {
                    let cells: Vec<_> = row
                        .iter()
                        .map(|cell| cell.replace('|', "\\|").replace(['\r', '\n'], " "))
                        .collect();
                    let _ = writeln!(text, "| {} |", cells.join(" | "));
                    if index == 0 {
                        let _ = writeln!(text, "|{}|", vec![" --- "; row.len()].join("|"));
                    }
                }
                text.push('\n');
            }
            _ => return Err(ToolError::Execution("invalid extracted DOCX block".into())),
        }
    }
    Ok(text)
}

/// Limits applied by every doc tool.
#[derive(Debug, Clone)]
pub struct DocToolsConfig {
    /// Largest input file accepted, in bytes (default 25 MiB).
    pub max_input_bytes: u64,
    /// Most rows / items placed in the structured JSON result (default 500).
    pub max_rows: usize,
    /// Longest markdown preview in characters (default 6000).
    pub max_preview_chars: usize,
    /// Longest single cell / paragraph kept in structured output (default 4000).
    pub max_cell_chars: usize,
    /// Largest written output (docx/xlsx/csv), in bytes (default 25 MiB).
    /// Also bounds DOCX read JSON, which has an additional hard 1 MiB cap.
    pub max_output_bytes: usize,
    /// Extra check on every output path, run by the writer itself. The daemon
    /// sets it to refuse its data directory and native CLI configuration, so a
    /// caller that skips the permission check still cannot plant a skill or a
    /// workflow there. It is a hook, not a dependency: `builtin-tools` already
    /// depends on this crate's consumers, so the guard cannot be imported here.
    pub write_guard: Option<WriteGuard>,
    /// Converter binaries, timeout and output folder for `doc_convert` / `doc_preview`.
    pub convert: ConvertConfig,
}

type GuardFn = dyn Fn(&str, u64) -> Result<(), String> + Send + Sync + std::panic::RefUnwindSafe;

/// A path check supplied by the embedder; `Err` carries the refusal reason.
#[derive(Clone)]
pub struct WriteGuard(Arc<GuardFn>);

impl WriteGuard {
    /// Wrap a check of the path alone.
    pub fn new(
        check: impl Fn(&str) -> Result<(), String> + Send + Sync + std::panic::RefUnwindSafe + 'static,
    ) -> Self {
        Self(Arc::new(move |path, _bytes| check(path)))
    }

    /// Wrap a check that also sees how many bytes the write adds to the folder (the new size
    /// less the size of the file it replaces, `0` for a read check), so a workspace quota is
    /// judged against the real size and not only against what is already there.
    pub fn sized(
        check: impl Fn(&str, u64) -> Result<(), String>
        + Send
        + Sync
        + std::panic::RefUnwindSafe
        + 'static,
    ) -> Self {
        Self(Arc::new(check))
    }

    pub(crate) fn check(&self, path: &str) -> Result<(), String> {
        (self.0)(path, 0)
    }

    pub(crate) fn check_sized(&self, path: &str, added_bytes: u64) -> Result<(), String> {
        (self.0)(path, added_bytes)
    }
}

impl std::fmt::Debug for WriteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteGuard")
    }
}

impl Default for DocToolsConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 25 * 1024 * 1024,
            max_rows: 500,
            max_preview_chars: 6000,
            max_cell_chars: 4000,
            max_output_bytes: 25 * 1024 * 1024,
            write_guard: None,
            convert: ConvertConfig::default(),
        }
    }
}

type Handler = fn(&DocToolsConfig, Value) -> Result<Reply, ToolError>;

struct DocTool {
    spec: ToolSpec,
    cfg: Arc<DocToolsConfig>,
    handler: Handler,
}

#[async_trait]
impl Tool for DocTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let cfg = self.cfg.clone();
        let handler = self.handler;
        let reply = tokio::task::spawn_blocking(move || handler(&cfg, input))
            .await
            .map_err(|e| ToolError::Execution(format!("doc tool task failed: {e}")))??;
        Ok(reply.into_output())
    }
}

/// Build all document tools.
#[must_use]
pub fn tools(cfg: &DocToolsConfig) -> Vec<Arc<dyn Tool>> {
    let cfg = Arc::new(cfg.clone());
    let defs: Vec<(ToolSpec, Handler)> = vec![
        (pdf::spec(), pdf::run),
        (docx::read_spec(), docx::run_read),
        (docx::write_spec(), docx::run_write),
        (xlsx::read_spec(), xlsx::run_read),
        (xlsx::write_spec(), xlsx::run_write),
        (xlsx_edit::append_spec(), xlsx_edit::run_append),
        (xlsx_edit::update_spec(), xlsx_edit::run_update),
        (docx_edit::append_spec(), docx_edit::run_append),
        (pptx::read_spec(), pptx::run_read),
        (csv_tools::read_spec(), csv_tools::run_read),
        (csv_tools::transform_spec(), csv_tools::run_transform),
        (json_query::spec(), json_query::run),
        (convert::convert_spec(), convert::run_convert),
        (convert::preview_spec(), convert::run_preview),
    ];
    defs.into_iter()
        .map(|(spec, handler)| {
            Arc::new(DocTool {
                spec,
                cfg: cfg.clone(),
                handler,
            }) as Arc<dyn Tool>
        })
        .collect()
}

/// Register every document tool in `registry`.
pub fn register(registry: &ToolRegistry, cfg: &DocToolsConfig) {
    for tool in tools(cfg) {
        registry.register(tool);
    }
}

/// Names of all tools in this crate.
pub const TOOL_NAMES: [&str; 14] = [
    "pdf_extract",
    "docx_read",
    "docx_write",
    "xlsx_read",
    "xlsx_write",
    "xlsx_append_rows",
    "xlsx_update_cells",
    "docx_append",
    "pptx_read",
    "csv_read",
    "csv_transform",
    "json_query",
    "doc_convert",
    "doc_preview",
];

pub(crate) fn mk_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    effect: Effect,
) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        effect,
        category: PermissionCategory::Fs,
        platforms: vec![Platform::All],
        content: match name {
            // Documents can carry prompt injection and are private files.
            "pdf_extract" | "docx_read" | "xlsx_read" | "csv_read" | "csv_transform"
            | "json_query" | "doc_preview" | "pptx_read" => {
                tool_registry::ContentClass::untrusted_private()
            }
            // The model already holds the document it is writing; the edit tools only
            // report what they changed, never what the file held.
            "docx_write" | "xlsx_write" | "doc_convert" | "xlsx_append_rows"
            | "xlsx_update_cells" | "docx_append" => tool_registry::ContentClass::none(),
            _ => tool_registry::ContentClass::unspecified(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_with_forged_sizes_is_refused_on_actual_bytes() {
        use std::io::Write as _;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            w.start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            let chunk = vec![b'a'; 1024 * 1024];
            for _ in 0..70 {
                w.write_all(&chunk).unwrap();
            }
            w.finish().unwrap();
        }
        let mut bytes = buf.into_inner();
        assert!(
            check_zip_limits(&bytes).is_err(),
            "honest header is refused"
        );
        // Forge the uncompressed size down to 1000 in the local and central headers.
        for (sig, at) in [(b"PK\x03\x04", 22), (b"PK\x01\x02", 24)] {
            let pos = bytes.windows(4).position(|w| w == sig).unwrap();
            bytes[pos + at..pos + at + 4].copy_from_slice(&1000u32.to_le_bytes());
        }
        assert!(
            check_zip_limits(&bytes).is_err(),
            "forged header is refused"
        );
    }

    #[test]
    fn extraction_limiter_bounds_in_flight_work() {
        static L: ExtractionLimiter = ExtractionLimiter::new(2);
        let a = L.try_acquire().unwrap();
        let _b = L.try_acquire().unwrap();
        assert!(L.try_acquire().is_none());
        drop(a);
        assert!(L.try_acquire().is_some());
    }

    #[test]
    fn registers_all_tools_with_valid_specs() {
        let reg = ToolRegistry::new();
        register(&reg, &DocToolsConfig::default());
        for name in TOOL_NAMES {
            let t = reg.get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(t.spec().is_valid(), "{name}");
        }
        assert_eq!(reg.get("docx_write").unwrap().spec().effect, Effect::Write);
        assert!(!reg.get("docx_write").unwrap().spec().content.reads_private);
        assert!(!reg.get("xlsx_write").unwrap().spec().content.reads_private);
        assert_eq!(reg.get("pdf_extract").unwrap().spec().effect, Effect::Read);
    }

    #[test]
    fn edit_and_read_tools_carry_the_right_metadata() {
        let reg = ToolRegistry::new();
        register(&reg, &DocToolsConfig::default());
        for name in ["xlsx_append_rows", "xlsx_update_cells", "docx_append"] {
            let spec = reg.get(name).unwrap().spec().clone();
            assert_eq!(spec.effect, Effect::Write, "{name}");
            assert_eq!(spec.category, PermissionCategory::Fs, "{name}");
            assert!(
                !spec.content.reads_private,
                "{name} must not echo file content"
            );
            assert!(
                spec.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f == "path")
            );
        }
        let pptx = reg.get("pptx_read").unwrap().spec().clone();
        assert_eq!(pptx.effect, Effect::Read);
        assert_eq!(pptx.category, PermissionCategory::Fs);
        assert!(pptx.content.ingests_untrusted && pptx.content.reads_private);
    }

    #[test]
    fn write_file_refuses_the_daemon_data_directory() {
        let data = tempfile::tempdir().unwrap();
        let root = data.path().to_path_buf();
        let cfg = DocToolsConfig {
            write_guard: Some(WriteGuard::new(move |path| {
                if std::path::Path::new(path).starts_with(&root) {
                    Err("daemon data is not writable".into())
                } else {
                    Ok(())
                }
            })),
            ..DocToolsConfig::default()
        };
        let inside = data.path().join("skills.md");
        let err = crate::common::write_file(&cfg, inside.to_str().unwrap(), b"hi", true)
            .expect_err("data dir");
        assert!(err.to_string().contains("daemon"), "{err}");
        assert!(!inside.exists());
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("ok.txt");
        crate::common::write_file(&cfg, path.to_str().unwrap(), b"hi", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hi");
    }
}

#[cfg(test)]
mod call_tests {
    use super::*;
    use serde_json::json;
    use tool_registry::ContentPart;

    #[tokio::test]
    async fn call_returns_markdown_and_json_parts() {
        let reg = ToolRegistry::new();
        register(&reg, &DocToolsConfig::default());
        let tool = reg.get("json_query").unwrap();
        let out = tool
            .call(
                &ToolContext::default(),
                json!({"json": {"a": [1, 2]}, "query": "$.a[*]"}),
            )
            .await
            .unwrap();
        assert!(matches!(&out.content[0], ContentPart::Text { text } if text.contains("2 match")));
        assert!(matches!(&out.content[1], ContentPart::Json { data } if data["total"] == 2));
        let err = tool
            .call(&ToolContext::default(), json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}

#[cfg(test)]
mod attachment_tests {
    use std::io::Write as _;

    use super::*;

    fn cfg() -> DocToolsConfig {
        DocToolsConfig::default()
    }

    fn zip_of(entries: &[(&str, String)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn pdf_text_layer_is_extracted() {
        let pdf = crate::pdf::tests::make_pdf(&[&["Quarterly pdf fixture"]]);
        let t = attachment_text(&cfg(), "pdf", "a.pdf", &pdf, 10_000).unwrap();
        assert!(t.contains("Quarterly pdf fixture"), "{t}");
    }

    #[test]
    fn docx_paragraphs_are_extracted() {
        let bytes = crate::docx::build_docx(&[crate::docx::WBlock::Paragraph {
            text: "Hello docx fixture".into(),
            bold: false,
        }])
        .unwrap();
        let t = attachment_text(&cfg(), "docx", "a.docx", &bytes, 10_000).unwrap();
        assert!(t.contains("Hello docx fixture"), "{t}");
    }

    #[test]
    fn xlsx_cells_are_extracted() {
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        ws.write_string(0, 0, "Item").unwrap();
        ws.write_string(0, 1, "Cell xlsx fixture").unwrap();
        let bytes = wb.save_to_buffer().unwrap();
        let t = attachment_text(&cfg(), "xlsx", "a.xlsx", &bytes, 10_000).unwrap();
        assert!(t.contains("Cell xlsx fixture"), "{t}");
    }

    #[test]
    fn pptx_slide_text_is_extracted_and_cut_at_the_limit() {
        let slide = format!(
            "<p:sld xmlns:a=\"a\" xmlns:p=\"p\"><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:sld>",
            "slide text ".repeat(50)
        );
        let bytes = zip_of(&[("ppt/slides/slide1.xml", slide)]);
        let t = attachment_text(&cfg(), "pptx", "a.pptx", &bytes, 100).unwrap();
        assert!(
            t.contains("slide text") && t.ends_with("[truncated]"),
            "{t}"
        );
    }

    #[test]
    fn an_archive_that_expands_beyond_the_limits_is_refused() {
        // 70 MiB of zeros compresses to a few KiB: the classic bomb shape.
        let bomb = zip_of(&[("ppt/slides/slide1.xml", " ".repeat(70 * 1024 * 1024))]);
        assert!(bomb.len() < 1024 * 1024);
        let err = attachment_text(&cfg(), "pptx", "b.pptx", &bomb, 10_000).unwrap_err();
        assert!(err.to_string().contains("size limit"), "{err}");
        let many: Vec<(String, String)> = (0..=MAX_ZIP_ENTRIES)
            .map(|n| (format!("p{n}.xml"), String::new()))
            .collect();
        let refs: Vec<(&str, String)> = many.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let err = attachment_text(&cfg(), "docx", "m.docx", &zip_of(&refs), 10_000).unwrap_err();
        assert!(err.to_string().contains("too many parts"), "{err}");
    }
}
