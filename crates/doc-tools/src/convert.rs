//! `doc_convert` and `doc_preview`: convert Office files to and from PDF and render
//! page previews.
//!
//! The heavy lifting is done by LibreOffice (`soffice --headless --convert-to`) when
//! it is installed (the "office pack"), and by `pdftoppm` (poppler) for page images.
//! A pure-Rust fallback exists only for PDF to DOCX (plain text); every other
//! conversion without LibreOffice fails with a plain "install the office pack" message.
//!
//! Safety rules, all enforced here:
//! - the document is *untrusted*: it is copied into a private temp directory and only
//!   that copy is handed to the converter;
//! - every call gets its own throw-away LibreOffice profile (`-env:UserInstallation`),
//!   `--safe-mode`, a wall-clock timeout (the whole process tree is killed) and a cleared
//!   environment (no proxy settings, no tokens);
//! - macro-enabled files (`.docm`, `.xlsm`, ... and any archive that carries a VBA or
//!   Basic project) and legacy binary formats are refused;
//! - input and output sizes are capped, the result is written only through the shared
//!   `write_file` (data-directory guard, no overwrite without `overwrite: true`) and,
//!   when `output_root` is set, only inside that folder.

use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, exec, invalid, parse, parse_ranges, read_limited, write_file};

/// Where the converters live and how far they may go. All values are owner-tunable.
#[derive(Debug, Clone)]
pub struct ConvertConfig {
    /// `soffice` binary. `None`: look on `PATH` and in the usual install folders.
    pub soffice_path: Option<PathBuf>,
    /// `pdftoppm` binary (poppler). `None`: look on `PATH`.
    pub pdftoppm_path: Option<PathBuf>,
    /// Longest a single converter run may take (default 120 s).
    pub timeout: Duration,
    /// When set, every file the tools write must be inside this folder (the bot workspace).
    pub output_root: Option<PathBuf>,
    /// Most preview pages per call (default 5, hard cap 10).
    pub preview_max_pages: u32,
    /// Preview resolution in dots per inch (default 110).
    pub preview_dpi: u32,
    /// Most preview bytes returned inline to the model (default 1.5 MiB).
    pub preview_inline_bytes: usize,
}

impl Default for ConvertConfig {
    fn default() -> Self {
        Self {
            soffice_path: None,
            pdftoppm_path: None,
            timeout: Duration::from_secs(120),
            output_root: None,
            preview_max_pages: 5,
            preview_dpi: 110,
            preview_inline_bytes: 1536 * 1024,
        }
    }
}

const HARD_MAX_PAGES: u32 = 10;
const NEED_OFFICE_PACK: &str = "LibreOffice is not installed here. Ask the owner to install the \"office pack\" (LibreOffice) for this computer or server, then try again";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Writer,
    Calc,
    Impress,
    Pdf,
}

fn family(ext: &str) -> Option<Family> {
    Some(match ext {
        "docx" | "odt" | "rtf" => Family::Writer,
        "xlsx" | "ods" => Family::Calc,
        "pptx" | "odp" => Family::Impress,
        "pdf" => Family::Pdf,
        _ => return None,
    })
}

/// Lower-case extension of `path`.
fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_macro_or_legacy(ext: &str) -> Option<&'static str> {
    match ext {
        "docm" | "dotm" | "xlsm" | "xltm" | "xlam" | "xlsb" | "pptm" | "potm" | "ppsm" | "ppam" => {
            Some("macro-enabled files are never opened")
        }
        "doc" | "xls" | "ppt" => Some(
            "old binary Office files can hide macros; save it as docx/xlsx/pptx first and try again",
        ),
        _ => None,
    }
}

/// Refuse a document that is not what its extension says or that carries macros.
fn vet_source(ext: &str, bytes: &[u8]) -> Result<(), ToolError> {
    match ext {
        "pdf" => {
            if !bytes.starts_with(b"%PDF") {
                return Err(invalid("the file is not a valid PDF"));
            }
        }
        "rtf" => {
            if !bytes.starts_with(b"{\\rtf") {
                return Err(invalid("the file is not a valid RTF document"));
            }
        }
        _ => {
            crate::check_zip_limits(bytes)?;
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                .map_err(|_| invalid("the file is not a valid Office document"))?;
            for name in zip
                .file_names()
                .filter_map(Result::ok)
                .map(|n| n.to_ascii_lowercase())
            {
                if name.ends_with("vbaproject.bin")
                    || name.starts_with("basic/")
                    || name.starts_with("scripts/")
                {
                    return Err(invalid("the document contains macros; it is not converted"));
                }
            }
            if let Ok(entry) = zip.by_name("[Content_Types].xml") {
                let mut head = Vec::new();
                let _ = entry.take(256 * 1024).read_to_end(&mut head);
                if String::from_utf8_lossy(&head).contains("macroEnabled") {
                    return Err(invalid("the document contains macros; it is not converted"));
                }
            }
        }
    }
    Ok(())
}

/// Whether `from` can be turned into `to`.
fn pair_supported(from: &str, to: &str) -> bool {
    let (Some(f), Some(t)) = (family(from), family(to)) else {
        return false;
    };
    if from == to {
        return false;
    }
    match (f, t) {
        (Family::Pdf, Family::Writer) => to != "rtf",
        (_, Family::Pdf) => f != Family::Pdf,
        (a, b) => a == b && a != Family::Pdf,
    }
}

fn convert_filter(to: &str) -> &'static str {
    match to {
        "pdf" => "pdf",
        "docx" => "docx:MS Word 2007 XML",
        "xlsx" => "xlsx:Calc MS Excel 2007 XML",
        "pptx" => "pptx:Impress MS PowerPoint 2007 XML",
        "odt" => "odt",
        "ods" => "ods",
        "odp" => "odp",
        _ => "rtf",
    }
}

// ---------------------------------------------------------------- tool lookup

fn find_in_path(names: &[&str]) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in names {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn soffice(cfg: &DocToolsConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.convert.soffice_path {
        return p.is_file().then(|| p.clone());
    }
    find_in_path(&["soffice", "soffice.exe", "libreoffice"]).or_else(|| {
        [
            r"C:\Program Files\LibreOffice\program\soffice.exe",
            r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
            "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    })
}

fn pdftoppm(cfg: &DocToolsConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.convert.pdftoppm_path {
        return p.is_file().then(|| p.clone());
    }
    find_in_path(&["pdftoppm", "pdftoppm.exe"])
}

// ------------------------------------------------------------ process running

fn percent_path_url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.:~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Environment the converters may see: nothing that could carry a proxy or a token.
fn clean_env(cmd: &mut Command, home: &Path) {
    cmd.env_clear();
    for key in [
        "PATH",
        "SystemRoot",
        "SYSTEMROOT",
        "ComSpec",
        "PATHEXT",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("TMPDIR", home)
        .env("TEMP", home)
        .env("TMP", home);
}

fn kill_tree(id: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &id.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{id}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Run `cmd` with no stdin and a deadline. On a timeout the whole process tree is killed.
/// Returns the tail of stderr when the command fails.
fn run_limited(mut cmd: Command, work: &Path, timeout: Duration) -> Result<(), String> {
    let err_path = work.join("stderr.txt");
    let err_file = fs::File::create(&err_path).map_err(|e| format!("temp file: {e}"))?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the converter: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut tail = fs::read_to_string(&err_path).unwrap_or_default();
                tail = tail.chars().take(300).collect();
                return Err(format!("the converter failed ({status}) {}", tail.trim()));
            }
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the converter took longer than {} seconds and was stopped",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("converter wait failed: {e}")),
        }
    }
}

// ------------------------------------------------------------------ path rules

/// Refuse `path` unless it is inside `output_root` (when one is configured).
fn confine(cfg: &DocToolsConfig, path: &str) -> Result<(), ToolError> {
    let Some(root) = &cfg.convert.output_root else {
        return Ok(());
    };
    let root = root
        .canonicalize()
        .map_err(|_| exec("the bot workspace folder does not exist"))?;
    let p = Path::new(path);
    let mut existing = p;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            break;
        };
        rest.push(name);
        existing = parent;
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(invalid("output paths may not contain '..'"));
    }
    let mut full = existing
        .canonicalize()
        .map_err(|_| exec("the output folder is not reachable"))?;
    for name in rest.iter().rev() {
        full.push(name);
    }
    if full.starts_with(&root) {
        Ok(())
    } else {
        Err(exec(format!(
            "output must stay inside the bot workspace ({})",
            root.display()
        )))
    }
}

fn default_output(path: &str, suffix: &str) -> String {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("converted");
    p.with_file_name(format!("{stem}{suffix}"))
        .to_string_lossy()
        .into_owned()
}

fn check_free(cfg: &DocToolsConfig, out: &str, overwrite: bool) -> Result<(), ToolError> {
    confine(cfg, out)?;
    if Path::new(out).exists() && !overwrite {
        return Err(exec(format!(
            "{out} already exists; pass overwrite:true to replace it"
        )));
    }
    Ok(())
}

// ------------------------------------------------------------------ soffice run

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new(source: &[u8], ext: &str) -> Result<(Self, PathBuf), ToolError> {
        let dir = tempfile::Builder::new()
            .prefix("praxi-convert-")
            .tempdir()
            .map_err(|e| exec(format!("cannot create a temp folder: {e}")))?;
        for sub in ["profile", "in", "out"] {
            fs::create_dir(dir.path().join(sub)).map_err(|e| exec(format!("temp folder: {e}")))?;
        }
        let input = dir.path().join("in").join(format!("input.{ext}"));
        fs::write(&input, source).map_err(|e| exec(format!("temp copy: {e}")))?;
        Ok((Self { dir }, input))
    }

    fn work(&self) -> &Path {
        self.dir.path()
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }
}

/// Run LibreOffice on `input`, producing `input.<out_ext>` in the sandbox `out` folder.
fn run_soffice(
    cfg: &DocToolsConfig,
    exe: &Path,
    sb: &Sandbox,
    input: &Path,
    convert_to: &str,
    out_ext: &str,
    pdf_source: bool,
) -> Result<PathBuf, ToolError> {
    let mut cmd = Command::new(exe);
    cmd.arg(format!(
        "-env:UserInstallation={}",
        percent_path_url(&sb.work().join("profile"))
    ))
    .args([
        "--headless",
        "--safe-mode",
        "--norestore",
        "--nolockcheck",
        "--nodefault",
        "--nologo",
    ]);
    if pdf_source {
        cmd.arg("--infilter=writer_pdf_import");
    }
    cmd.arg("--convert-to")
        .arg(convert_to)
        .arg("--outdir")
        .arg(sb.out())
        .arg(input)
        .current_dir(sb.work());
    clean_env(&mut cmd, &sb.work().join("profile"));
    run_limited(cmd, sb.work(), cfg.convert.timeout).map_err(exec)?;
    let produced = sb.out().join(format!("input.{out_ext}"));
    let len = fs::metadata(&produced)
        .map_err(|_| {
            exec("the converter finished but produced no file (is the document damaged?)")
        })?
        .len();
    if len == 0 {
        return Err(exec("the converter produced an empty file"));
    }
    if len > cfg.max_output_bytes as u64 {
        return Err(exec(format!(
            "the result is {len} bytes, over the {} byte limit",
            cfg.max_output_bytes
        )));
    }
    Ok(produced)
}

// --------------------------------------------------------------------- doc_convert

#[derive(Deserialize)]
struct ConvertIn {
    path: String,
    to: String,
    output_path: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

pub fn convert_spec() -> ToolSpec {
    crate::mk_spec(
        "doc_convert",
        "Convert a document between Office formats and PDF with LibreOffice: docx/odt/rtf, xlsx/ods and pptx/odp to pdf or to their sibling formats, and pdf to docx/odt. Needs the office pack; PDF to DOCX also works without it (plain text only). Macro files and old .doc/.xls/.ppt are refused. Never overwrites unless overwrite:true; the result must stay in the bot workspace. Example: {\"path\":\"offer.docx\",\"to\":\"pdf\"}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Source document."},
            "to":{"type":"string","enum":["pdf","docx","xlsx","pptx","odt","ods","odp","rtf"],"description":"Target format."},
            "output_path":{"type":"string","description":"Where to write the result (default: next to the source with the new extension)."},
            "overwrite":{"type":"boolean","description":"Replace an existing output file (default false)."}
        },"required":["path","to"]}),
        Effect::Write,
    )
}

pub fn run_convert(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: ConvertIn = parse(input)?;
    let from = ext_of(&i.path);
    let to = i.to.trim().trim_start_matches('.').to_ascii_lowercase();
    if let Some(why) = is_macro_or_legacy(&from) {
        return Err(invalid(why));
    }
    if !pair_supported(&from, &to) {
        return Err(invalid(format!(
            "cannot convert .{from} to .{to}; supported: docx/odt/rtf, xlsx/ods and pptx/odp to pdf or a sibling format, and pdf to docx/odt"
        )));
    }
    let out_path = i
        .output_path
        .clone()
        .unwrap_or_else(|| default_output(&i.path, &format!(".{to}")));
    if ext_of(&out_path) != to {
        return Err(invalid(format!("output_path must end in .{to}")));
    }
    check_free(cfg, &out_path, i.overwrite)?;
    let bytes = read_limited(cfg, &i.path)?;
    vet_source(&from, &bytes)?;

    let (result, engine, note) = if let Some(exe) = soffice(cfg) {
        let (sb, input_copy) = Sandbox::new(&bytes, &from)?;
        let produced = run_soffice(
            cfg,
            &exe,
            &sb,
            &input_copy,
            convert_filter(&to),
            &to,
            from == "pdf",
        )?;
        let data = fs::read(&produced).map_err(|e| exec(format!("cannot read the result: {e}")))?;
        (data, "libreoffice", "")
    } else if from == "pdf" && to == "docx" {
        (
            pdf_to_docx_text(cfg, &i.path, &bytes)?,
            "built-in",
            " Plain text only: layout, tables and images were not kept; the office pack gives a better result.",
        )
    } else {
        return Err(exec(NEED_OFFICE_PACK));
    };
    write_file(cfg, &out_path, &result, i.overwrite)?;
    let md = format!(
        "Converted {} to {out_path} ({} bytes, {engine}).{note}\n",
        i.path,
        result.len()
    );
    let data =
        json!({"path": out_path, "bytes": result.len(), "engine": engine, "from": from, "to": to});
    Ok(Reply::new(cfg, &md, data))
}

/// Pure-Rust PDF to DOCX: the text layer, one paragraph per block of lines.
fn pdf_to_docx_text(cfg: &DocToolsConfig, label: &str, bytes: &[u8]) -> Result<Vec<u8>, ToolError> {
    let reply = crate::pdf::from_bytes(cfg, label, bytes)?;
    let mut blocks = Vec::new();
    for page in reply.data["pages"].as_array().into_iter().flatten() {
        let text = page["text"].as_str().unwrap_or_default();
        let mut para = String::new();
        for line in text.lines().chain(std::iter::once("")) {
            if line.trim().is_empty() {
                if !para.is_empty() {
                    blocks.push(crate::docx::WBlock::Paragraph {
                        text: std::mem::take(&mut para),
                        bold: false,
                    });
                }
            } else {
                if !para.is_empty() {
                    para.push(' ');
                }
                para.push_str(line.trim());
            }
        }
    }
    if blocks.is_empty() {
        return Err(exec(
            "the PDF has no text layer (a scan?); install the office pack to convert it",
        ));
    }
    crate::docx::build_docx(&blocks)
}

// -------------------------------------------------------------------- doc_preview

#[derive(Deserialize)]
struct PreviewIn {
    path: String,
    pages: Option<String>,
    dpi: Option<u32>,
    output_dir: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

pub fn preview_spec() -> ToolSpec {
    crate::mk_spec(
        "doc_preview",
        "Render the first pages of a PDF or Office document as PNG images so you can look at the real layout. Saves <name>-p<N>.png files (default: next to the document) and returns the images. Needs pdftoppm (docs pack) and/or LibreOffice (office pack). Example: {\"path\":\"offer.docx\",\"pages\":\"1-2\"}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"PDF, docx/odt/rtf, xlsx/ods or pptx/odp file."},
            "pages":{"type":"string","description":"1-based pages, e.g. \"1-3\". Default: the first pages."},
            "dpi":{"type":"integer","minimum":50,"maximum":200,"description":"Resolution (default 110)."},
            "output_dir":{"type":"string","description":"Folder for the PNG files (default: the document's folder)."},
            "overwrite":{"type":"boolean","description":"Replace existing preview files (default false)."}
        },"required":["path"]}),
        Effect::Write,
    )
}

fn page_number(file: &str) -> Option<u32> {
    file.strip_suffix(".png")?.rsplit('-').next()?.parse().ok()
}

pub fn run_preview(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: PreviewIn = parse(input)?;
    let from = ext_of(&i.path);
    if let Some(why) = is_macro_or_legacy(&from) {
        return Err(invalid(why));
    }
    if family(&from).is_none() {
        return Err(invalid(format!("cannot preview .{from} files")));
    }
    let max_pages = cfg.convert.preview_max_pages.clamp(1, HARD_MAX_PAGES);
    let spec = i.pages.clone().unwrap_or_else(|| format!("1-{max_pages}"));
    let wanted: Vec<u32> = parse_ranges(&spec, 100_000)?
        .into_iter()
        .take(max_pages as usize)
        .collect();
    let (Some(&first), Some(&last)) = (wanted.first(), wanted.last()) else {
        return Err(invalid("no pages requested"));
    };
    let dpi = i.dpi.unwrap_or(cfg.convert.preview_dpi).clamp(50, 200);
    let out_dir = i.output_dir.clone().unwrap_or_else(|| {
        Path::new(&i.path)
            .parent()
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned())
    });
    let stem = Path::new(&i.path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document")
        .to_owned();
    let target = |n: u32| {
        Path::new(&out_dir)
            .join(format!("{stem}-p{n}.png"))
            .to_string_lossy()
            .into_owned()
    };
    for n in &wanted {
        check_free(cfg, &target(*n), i.overwrite)?;
    }
    let bytes = read_limited(cfg, &i.path)?;
    vet_source(&from, &bytes)?;

    let ppm = pdftoppm(cfg);
    let office = soffice(cfg);
    let (sb, input_copy) = Sandbox::new(&bytes, &from)?;
    let mut produced: Vec<(u32, PathBuf)> = Vec::new();
    if let Some(ppm) = ppm {
        let pdf = if from == "pdf" {
            input_copy
        } else if let Some(exe) = &office {
            run_soffice(cfg, exe, &sb, &input_copy, "pdf", "pdf", false)?
        } else {
            return Err(exec(NEED_OFFICE_PACK));
        };
        let mut cmd = Command::new(ppm);
        cmd.args([
            "-png",
            "-r",
            &dpi.to_string(),
            "-f",
            &first.to_string(),
            "-l",
        ])
        .arg(last.to_string())
        .arg(&pdf)
        .arg(sb.out().join("page"))
        .current_dir(sb.work());
        clean_env(&mut cmd, sb.work());
        run_limited(cmd, sb.work(), cfg.convert.timeout).map_err(exec)?;
        for entry in fs::read_dir(sb.out()).map_err(|e| exec(format!("preview folder: {e}")))? {
            let entry = entry.map_err(|e| exec(format!("preview folder: {e}")))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(n) = page_number(&name).filter(|n| wanted.contains(n)) {
                produced.push((n, entry.path()));
            }
        }
    } else if let (true, Some(exe)) = (from != "pdf", &office) {
        // No pdftoppm: LibreOffice can still draw the first page.
        let png = run_soffice(cfg, exe, &sb, &input_copy, "png", "png", false)?;
        produced.push((1, png));
    } else {
        return Err(exec(
            "no page renderer is installed: ask the owner to install the \"docs pack\" (pdftoppm) or the office pack (LibreOffice)",
        ));
    }
    produced.sort_by_key(|(n, _)| *n);
    if produced.is_empty() {
        return Err(exec("the document has none of the requested pages"));
    }
    let mut files = Vec::new();
    let mut inline = Vec::new();
    let mut inline_budget = cfg.convert.preview_inline_bytes;
    for (n, tmp) in &produced {
        let png = fs::read(tmp).map_err(|e| exec(format!("cannot read a preview: {e}")))?;
        let dest = target(*n);
        write_file(cfg, &dest, &png, i.overwrite)?;
        files.push(json!({"page": n, "path": dest, "bytes": png.len()}));
        if png.len() <= inline_budget {
            inline_budget -= png.len();
            inline.push(base64::engine::general_purpose::STANDARD.encode(&png));
        }
    }
    let md = format!(
        "Rendered {} page(s) of {} at {dpi} dpi:\n{}\n",
        files.len(),
        i.path,
        files
            .iter()
            .map(|f| format!("- {}", f["path"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut reply = Reply::new(cfg, &md, json!({"files": files, "dpi": dpi}));
    reply.images = inline;
    Ok(reply)
}
