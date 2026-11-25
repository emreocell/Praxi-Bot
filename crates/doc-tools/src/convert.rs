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
//!   `write_file` (data-directory guard, no overwrite without `overwrite: true`) and only
//!   inside `output_root`;
//! - a document that refers to anything outside itself (linked images, templates, OLE,
//!   DDE, ODF `xlink:href`, mail merge) is refused before the converter starts (see
//!   `convert_vet`), and the throw-away profile is pre-seeded to block macros, link
//!   updates and network access;
//! - `output_root` and `read_guard` are mandatory: without them both tools fail closed.
//!   The input must not be a symlink / junction, and a link anywhere above it may not lead
//!   out of `output_root`;
//! - NETWORK ISOLATION is the real control (see `convert_sandbox`): on Linux the converters
//!   run in a network-less namespace (bubblewrap, `unshare -rn` / `-n`), and without one
//!   the tools fail closed unless the owner sets `allow_unsandboxed_convert`. On Windows
//!   conversion is off unless `convert_enabled_on_windows` is set. Vetting (`convert_vet`)
//!   is a small deny-by-default list on top of that, not a substitute for it;
//! - one conversion runs at a time (a global slot), previews are pixel-, page- and
//!   byte-bounded, and the converter is killed when it floods its output folder.
//!
//! Trust: the converter binaries are found by the configured `soffice_path` /
//! `pdftoppm_path`, then in fixed install locations, then on `PATH`. Empty, relative and
//! current-directory `PATH` entries are ignored, but an absolute, user-writable `PATH`
//! entry is trusted: whoever can write there can already run code as the daemon.
//! PDF input is not decoded in this process: the converter reads it under the input size
//! cap, the timeout and the output-folder cap; only the built-in PDF to DOCX fallback
//! parses it here, under the extraction budget of `pdf_budget`.

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

use crate::common::{Reply, exec, invalid, parse, parse_ranges, read_limited, write_file};
use crate::convert_sandbox;
use crate::convert_vet::vet_source;
use crate::{DocToolsConfig, WriteGuard};

/// Where the converters live and how far they may go. All values are owner-tunable.
#[derive(Debug, Clone)]
pub struct ConvertConfig {
    /// `soffice` binary. `None`: look on `PATH` and in the usual install folders.
    pub soffice_path: Option<PathBuf>,
    /// `pdftoppm` binary (poppler). `None`: look on `PATH`.
    pub pdftoppm_path: Option<PathBuf>,
    /// Longest a single converter run may take (default 120 s).
    pub timeout: Duration,
    /// The bot workspace. MANDATORY: every file the tools write must be inside this
    /// folder, and the tools refuse to run while it is unset.
    pub output_root: Option<PathBuf>,
    /// MANDATORY check on the input path (same rule as `fs_read`: credential folders,
    /// the daemon data directory, ...). It is a hook because `builtin-tools` cannot be
    /// imported here; it runs on the given path and on its resolved location. The tools
    /// refuse to run while it is unset.
    pub read_guard: Option<WriteGuard>,
    /// Most preview pages per call (default 5, hard cap 10).
    pub preview_max_pages: u32,
    /// Preview resolution in dots per inch (default 110).
    pub preview_dpi: u32,
    /// Most preview bytes returned inline to the model (default 1.5 MiB).
    pub preview_inline_bytes: usize,
    /// Run the converters even when no network sandbox is available (Linux without
    /// bubblewrap / user namespaces, macOS). UNSAFE for untrusted documents: a crafted
    /// file may make the converter contact a server. Default `false`: fail closed.
    pub allow_unsandboxed_convert: bool,
    /// Windows has no network sandbox, so conversion is off there unless this is set.
    /// Enabling it exposes the daemon to NTLM / SMB leaks from crafted documents that
    /// the vetting misses; use it only for documents you trust. Default `false`.
    pub convert_enabled_on_windows: bool,
}

impl Default for ConvertConfig {
    fn default() -> Self {
        Self {
            soffice_path: None,
            pdftoppm_path: None,
            timeout: Duration::from_secs(120),
            output_root: None,
            read_guard: None,
            preview_max_pages: 5,
            preview_dpi: 110,
            preview_inline_bytes: 1536 * 1024,
            allow_unsandboxed_convert: false,
            convert_enabled_on_windows: false,
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
        // `rtf` is an output format only; RTF input is refused (see `convert_vet`).
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

/// Whether `from` can be turned into `to`.
fn pair_supported(from: &str, to: &str) -> bool {
    let (Some(f), Some(t)) = (family(from), family(to)) else {
        return false;
    };
    if from == to || from == "rtf" {
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

/// First existing `name` in the absolute, non-current directories of `paths`.
/// Empty and relative entries (which mean "the current directory") are skipped.
pub(crate) fn find_in(paths: &std::ffi::OsStr, names: &[&str]) -> Option<PathBuf> {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    for dir in std::env::split_paths(paths) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        if cwd.is_some() && dir.canonicalize().ok() == cwd {
            continue;
        }
        for name in names {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn find_in_path(names: &[&str]) -> Option<PathBuf> {
    find_in(&std::env::var_os("PATH")?, names)
}

fn soffice(cfg: &DocToolsConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.convert.soffice_path {
        return p.is_file().then(|| p.clone());
    }
    // Fixed install locations first, `PATH` last.
    [
        r"C:\Program Files\LibreOffice\program\soffice.exe",
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        "/usr/bin/soffice",
        "/usr/lib/libreoffice/program/soffice",
        "/opt/libreoffice/program/soffice",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .or_else(|| find_in_path(&["soffice", "soffice.exe", "libreoffice"]))
}

fn pdftoppm(cfg: &DocToolsConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.convert.pdftoppm_path {
        return p.is_file().then(|| p.clone());
    }
    find_in_path(&["pdftoppm", "pdftoppm.exe"])
}

// ---------------------------------------------------------- one conversion at a time

static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Wait for the global conversion slot. Concurrent calls would each start a converter
/// that may run for the whole timeout; only one runs, the others wait (bounded).
fn acquire_slot(cfg: &DocToolsConfig) -> Result<std::sync::MutexGuard<'static, ()>, ToolError> {
    let give_up = Instant::now() + cfg.convert.timeout.max(Duration::from_secs(30));
    loop {
        match SLOT.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(p)) => return Ok(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= give_up {
                    return Err(exec(
                        "another document conversion is still running; try again in a moment",
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
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

/// Environment the converters may see: an allow-list of what they need to start. Nothing
/// that could carry a proxy, a certificate store, credentials or a token is passed
/// (`HTTP(S)_PROXY`, `SSL_CERT_*`, `AWS_*`, `*_TOKEN`, ... are all dropped by `env_clear`).
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

/// Pre-seed the throw-away profile so LibreOffice starts locked down even if a document
/// slips past `vet_source`: macros off (security level 3, "very high"), links never
/// updated, no Java, and every proxy pointed at a closed local port so that a stray
/// network request fails at once instead of leaving the machine. Keys LibreOffice does
/// not know are ignored; this is a second line, `vet_source` is the first.
fn seed_profile(profile: &Path) -> Result<(), ToolError> {
    const XCU: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="BlockUntrustedRefererLinks" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Office.Writer/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
<item oor:path="/org.openoffice.Office.Writer/Content/Update"><prop oor:name="Field" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Writer/Content/Update"><prop oor:name="Chart" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Calc/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
<item oor:path="/org.openoffice.Office.Java/VirtualMachine"><prop oor:name="Enable" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetProxyType" oor:op="fuse"><value>1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPProxyName" oor:op="fuse"><value>127.0.0.1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPProxyPort" oor:op="fuse"><value>9</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPSProxyName" oor:op="fuse"><value>127.0.0.1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPSProxyPort" oor:op="fuse"><value>9</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetFTPProxyName" oor:op="fuse"><value>127.0.0.1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetFTPProxyPort" oor:op="fuse"><value>9</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetNoProxy" oor:op="fuse"><value></value></prop></item>
</oor:items>
"#;
    let user = profile.join("user");
    fs::create_dir_all(&user).map_err(|e| exec(format!("temp folder: {e}")))?;
    fs::write(user.join("registrymodifications.xcu"), XCU)
        .map_err(|e| exec(format!("temp folder: {e}")))
}

#[cfg(unix)]
fn kill_tree(id: u32) {
    // The converter is its own process-group leader, so `-id` is the whole group.
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{id}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn kill_tree(id: u32) {
    // Absolute path: a bare name would be looked up in the current folder / `PATH`.
    // `/T` walks the live parent's children, so it only works while the parent exists;
    // the converter is the parent here and is killed first on every path that matters.
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = Path::new(&root).join("System32").join("taskkill.exe");
    let _ = Command::new(exe)
        .args(["/PID", &id.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Total size of the regular files under `dir` (bounded walk).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                return u64::MAX;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Most diagnostic text a converter may write.
const STDERR_CAP: u64 = 4 * 1024 * 1024;

/// Run `cmd` with no stdin and a deadline. On a timeout, or when it writes more than
/// the byte limit of any `limits` folder, the whole process tree is killed. On Unix the tree is
/// killed after a normal exit as well, so a lingering helper (`soffice.bin`) cannot
/// outlive the call. Returns the tail of stderr when the command fails.
fn run_limited(
    mut cmd: Command,
    work: &Path,
    timeout: Duration,
    limits: &[(&Path, u64)],
) -> Result<(), String> {
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
    let mut tick = 0u32;
    let stop = |child: &mut std::process::Child| {
        kill_tree(child.id());
        let _ = child.kill();
        let _ = child.wait();
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                #[cfg(unix)]
                kill_tree(child.id());
                if status.success() {
                    return Ok(());
                }
                let mut tail = fs::read_to_string(&err_path).unwrap_or_default();
                tail = tail.chars().take(300).collect();
                return Err(format!("the converter failed ({status}) {}", tail.trim()));
            }
            Ok(None) if Instant::now() >= deadline => {
                stop(&mut child);
                return Err(format!(
                    "the converter took longer than {} seconds and was stopped",
                    timeout.as_secs()
                ));
            }
            Ok(None) => {
                tick = tick.wrapping_add(1);
                if tick.is_multiple_of(8) {
                    for (dir, cap) in limits {
                        if dir_bytes(dir) > *cap {
                            stop(&mut child);
                            return Err(format!(
                                "the converter wrote more than the {cap} byte limit and was stopped"
                            ));
                        }
                    }
                    if fs::metadata(&err_path).is_ok_and(|m| m.len() > STDERR_CAP) {
                        stop(&mut child);
                        return Err(
                            "the converter wrote too much diagnostic output and was stopped".into(),
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("converter wait failed: {e}")),
        }
    }
}

// ------------------------------------------------------------------ path rules

/// The bot workspace, canonical. Fails closed when none is configured.
fn workspace(cfg: &DocToolsConfig) -> Result<PathBuf, ToolError> {
    let Some(root) = &cfg.convert.output_root else {
        return Err(exec(
            "document conversion is not available: no bot workspace folder is configured",
        ));
    };
    root.canonicalize()
        .map_err(|_| exec("the bot workspace folder does not exist"))
}

/// Both mandatory settings, before anything else happens.
fn require_setup(cfg: &DocToolsConfig) -> Result<PathBuf, ToolError> {
    convert_sandbox::platform_gate(cfg)?;
    let root = workspace(cfg)?;
    if cfg.convert.read_guard.is_none() {
        return Err(exec(
            "document conversion is not available: no input path guard is configured",
        ));
    }
    Ok(root)
}

/// Drop the `\\?\` prefix `canonicalize` adds on Windows so path guards see a normal path.
fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&s)
        .to_owned()
}

/// Refuse an input that is a link, that leads out of the workspace through a linked
/// folder, or that the read guard (same rule as `fs_read`) protects.
fn check_input(cfg: &DocToolsConfig, root: &Path, path: &str) -> Result<(), ToolError> {
    // First, by text alone: any filesystem call on a UNC / device path is a network call.
    convert_sandbox::guard_path(path)?;
    let p = Path::new(path);
    let meta = fs::symlink_metadata(p).map_err(|e| exec(format!("cannot stat {path}: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(invalid(format!(
            "{path} is a symbolic link or junction; give the path of the real file"
        )));
    }
    let resolved = p
        .canonicalize()
        .map_err(|e| exec(format!("cannot resolve {path}: {e}")))?;
    let via_link = p.ancestors().skip(1).any(|a| {
        !a.as_os_str().is_empty()
            && fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink())
    });
    if via_link && !resolved.starts_with(root) {
        return Err(exec(format!(
            "{path} goes through a link that leaves the bot workspace ({})",
            root.display()
        )));
    }
    if let Some(guard) = &cfg.convert.read_guard {
        guard.check(path).map_err(exec)?;
        guard.check(&plain(&resolved)).map_err(exec)?;
    }
    Ok(())
}

/// Refuse `path` unless it is inside the bot workspace.
fn confine(cfg: &DocToolsConfig, path: &str) -> Result<(), ToolError> {
    convert_sandbox::guard_path(path)?;
    let root = workspace(cfg)?;
    let p = Path::new(path);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(invalid("output paths may not contain '..'"));
    }
    // An existing or dangling link at the destination would redirect the write.
    if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(exec(format!(
            "{path} is a symbolic link or junction; output must stay inside the bot workspace ({})",
            root.display()
        )));
    }
    let mut existing = p;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            break;
        };
        rest.push(name);
        existing = parent;
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
        seed_profile(&dir.path().join("profile"))?;
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

/// Room for a converter's own profile and caches beyond the input and the output cap.
const WORK_SLACK: u64 = if cfg!(test) {
    64 * 1024
} else {
    256 * 1024 * 1024
};

/// Size limits of one run: the output folder, and the whole temp folder (profile, caches
/// and scratch files included) so a converter cannot fill the disk through them.
fn limits(cfg: &DocToolsConfig, sb: &Sandbox) -> [(PathBuf, u64); 2] {
    let cap = cfg.max_output_bytes as u64;
    let whole = cap
        .saturating_add(cfg.max_input_bytes)
        .saturating_add(WORK_SLACK);
    [(sb.out(), cap), (sb.work().to_path_buf(), whole)]
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
    let mut cmd = convert_sandbox::command(cfg, exe, sb.work())?;
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
    let cap = cfg.max_output_bytes as u64;
    run_limited(
        cmd,
        sb.work(),
        cfg.convert.timeout,
        &limits(cfg, sb).each_ref().map(|(p, c)| (p.as_path(), *c)),
    )
    .map_err(exec)?;
    let produced = sb.out().join(format!("input.{out_ext}"));
    let meta = fs::symlink_metadata(&produced).map_err(|_| {
        exec("the converter finished but produced no file (is the document damaged?)")
    })?;
    if !meta.is_file() {
        return Err(exec("the converter produced something that is not a file"));
    }
    let len = meta.len();
    if len == 0 {
        return Err(exec("the converter produced an empty file"));
    }
    if len > cap {
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
        "Convert a document between Office formats and PDF with LibreOffice: docx/odt, xlsx/ods and pptx/odp to pdf or to their sibling formats (rtf only as an output), and pdf to docx/odt. Needs the office pack; PDF to DOCX also works without it (plain text only). Macro files and old .doc/.xls/.ppt are refused. Never overwrites unless overwrite:true; the result must stay in the bot workspace. Example: {\"path\":\"offer.docx\",\"to\":\"pdf\"}",
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
    // Textual path guard before ANY filesystem call (UNC / device paths hit the network).
    convert_sandbox::guard_path(&i.path)?;
    if let Some(o) = &i.output_path {
        convert_sandbox::guard_path(o)?;
    }
    let root = require_setup(cfg)?;
    let from = ext_of(&i.path);
    let to = i.to.trim().trim_start_matches('.').to_ascii_lowercase();
    if let Some(why) = is_macro_or_legacy(&from) {
        return Err(invalid(why));
    }
    if !pair_supported(&from, &to) {
        return Err(invalid(format!(
            "cannot convert .{from} to .{to}; supported: docx/odt, xlsx/ods and pptx/odp to pdf or a sibling format, and pdf to docx/odt"
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
    check_input(cfg, &root, &i.path)?;
    let bytes = read_limited(cfg, &i.path)?;
    vet_source(&from, &bytes)?;
    let _slot = acquire_slot(cfg)?;

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
            "path":{"type":"string","description":"PDF, docx/odt, xlsx/ods or pptx/odp file."},
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

/// Longest side of an output image in pixels, whatever the page size and dpi.
const MAX_PREVIEW_PX: f64 = 2000.0;
/// Side used when the page size cannot be read cheaply from the PDF.
const UNKNOWN_PAGE_PX: u32 = 1600;
/// A page longer than this many points (200 inches) is refused as absurd.
const MAX_PAGE_POINTS: f64 = 14_400.0;
/// How much of the PDF is searched for page boxes.
const BOX_SCAN_BYTES: u64 = 8 * 1024 * 1024;

/// The largest `/MediaBox` or `/CropBox` side (in points) written in plain text in the
/// first bytes of the PDF. `None` when none is readable (object streams hide them); the
/// pixel cap then still applies.
fn page_extent(pdf: &Path) -> Option<f64> {
    let mut head = Vec::new();
    fs::File::open(pdf)
        .ok()?
        .take(BOX_SCAN_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    let mut best: Option<f64> = None;
    for key in [&b"/MediaBox"[..], &b"/CropBox"[..]] {
        let mut from = 0;
        while let Some(at) = head[from..].windows(key.len()).position(|w| w == key) {
            let start = from + at + key.len();
            from = start;
            let rest = &head[start..head.len().min(start + 120)];
            let text = String::from_utf8_lossy(rest);
            let Some(inner) = text.trim_start().strip_prefix('[') else {
                continue;
            };
            let nums: Vec<f64> = inner
                .split(']')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if let [x1, y1, x2, y2] = nums[..] {
                let side = (x2 - x1).abs().max((y2 - y1).abs());
                if side.is_nan() || side > f64::from(u32::MAX) {
                    return Some(f64::INFINITY);
                }
                best = Some(best.map_or(side, |b| b.max(side)));
            }
        }
    }
    best
}

/// Pixel length of the longer page side for `pdftoppm -scale-to`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scale_to(extent: Option<f64>, dpi: u32) -> u32 {
    extent.map_or(UNKNOWN_PAGE_PX, |pts| {
        // Clamped to 64..=2000 first, so the narrowing is exact.
        (pts / 72.0 * f64::from(dpi))
            .round()
            .clamp(64.0, MAX_PREVIEW_PX) as u32
    })
}

/// Group sorted page numbers into runs of consecutive pages, so that `1,99999` is two
/// one-page renders and never a hundred thousand pages.
fn runs(pages: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &n in pages {
        match out.last_mut() {
            Some((_, last)) if *last + 1 == n => *last = n,
            _ => out.push((n, n)),
        }
    }
    out
}

pub fn run_preview(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: PreviewIn = parse(input)?;
    convert_sandbox::guard_path(&i.path)?;
    if let Some(o) = &i.output_dir {
        convert_sandbox::guard_path(o)?;
    }
    let root = require_setup(cfg)?;
    let from = ext_of(&i.path);
    if let Some(why) = is_macro_or_legacy(&from) {
        return Err(invalid(why));
    }
    if family(&from).is_none() || from == "rtf" {
        return Err(invalid(format!("cannot preview .{from} files")));
    }
    let max_pages = cfg.convert.preview_max_pages.clamp(1, HARD_MAX_PAGES);
    let spec = i.pages.clone().unwrap_or_else(|| format!("1-{max_pages}"));
    let wanted: Vec<u32> = parse_ranges(&spec, 100_000)?
        .into_iter()
        .take(max_pages as usize)
        .collect();
    if wanted.is_empty() {
        return Err(invalid("no pages requested"));
    }
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
    check_input(cfg, &root, &i.path)?;
    let bytes = read_limited(cfg, &i.path)?;
    vet_source(&from, &bytes)?;
    let _slot = acquire_slot(cfg)?;

    let cap = cfg.max_output_bytes as u64;
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
        let extent = page_extent(&pdf);
        if extent.is_some_and(|pts| pts > MAX_PAGE_POINTS) {
            return Err(exec(
                "the document has an absurdly large page size and is not rendered",
            ));
        }
        let px = scale_to(extent, dpi);
        let started = Instant::now();
        for (run_no, (first, last)) in runs(&wanted).into_iter().enumerate() {
            let left = cfg.convert.timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(exec(format!(
                    "the converter took longer than {} seconds and was stopped",
                    cfg.convert.timeout.as_secs()
                )));
            }
            let mut cmd = convert_sandbox::command(cfg, &ppm, sb.work())?;
            cmd.args([
                "-png",
                "-scale-to",
                &px.to_string(),
                "-f",
                &first.to_string(),
                "-l",
            ])
            .arg(last.to_string())
            .arg(&pdf)
            .arg(sb.out().join(format!("r{run_no}")))
            .current_dir(sb.work());
            clean_env(&mut cmd, sb.work());
            run_limited(
                cmd,
                sb.work(),
                left,
                &limits(cfg, &sb).each_ref().map(|(p, c)| (p.as_path(), *c)),
            )
            .map_err(exec)?;
        }
        for entry in fs::read_dir(sb.out()).map_err(|e| exec(format!("preview folder: {e}")))? {
            let entry = entry.map_err(|e| exec(format!("preview folder: {e}")))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(n) = page_number(&name)
                .filter(|n| wanted.contains(n))
                .filter(|n| !produced.iter().any(|(m, _)| m == n))
            {
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
    // Size every produced file BEFORE reading any of them: each one and the sum must fit
    // the output limit, so a huge render can never be pulled into memory.
    let mut total = 0u64;
    for (_, tmp) in &produced {
        let meta = fs::symlink_metadata(tmp)
            .map_err(|e| exec(format!("cannot inspect a preview: {e}")))?;
        if !meta.is_file() {
            return Err(exec("the renderer produced something that is not a file"));
        }
        total = total.saturating_add(meta.len());
        if meta.len() > cap || total > cap {
            return Err(exec(format!(
                "the previews are over the {} byte limit",
                cfg.max_output_bytes
            )));
        }
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
