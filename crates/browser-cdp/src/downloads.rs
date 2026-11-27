//! Sandboxed downloads.
//!
//! A launched browser refuses every download (see [`crate::Browser::launch`]): a page must not
//! be able to put a file on the owner's disk. [`crate::Browser::allow_downloads`] turns that
//! into a narrow permission for ONE folder the caller chose (the bot's own workspace):
//!
//! * the browser itself names every file by a random id (`allowAndName`), so a page cannot pick
//!   a path, overwrite a file or smuggle `..` into a name; only after the download finished is
//!   the file renamed to a sanitised version of the name the page suggested;
//! * a file over the size cap, or one that would exceed the file-count cap, is cancelled while
//!   it is still arriving and never kept;
//! * a file whose type could be launched with a double click gets a `.download` suffix;
//! * nothing here ever opens a file.
//!
//! The pure parts (name, type, event handling) are plain functions so they are tested without
//! a browser; the real-browser tests live in `tests/downloads.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::client::CdpClient;
use crate::error::{Error, Result};

/// Where downloads may land and how much of them is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPolicy {
    /// The only folder a download is written to (created when missing).
    pub dir: PathBuf,
    /// A file larger than this is cancelled.
    pub max_file_bytes: u64,
    /// With this many files in the folder (finished or arriving) a new download is cancelled.
    pub max_files: usize,
}

impl DownloadPolicy {
    /// Largest file kept by default.
    pub const DEFAULT_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
    /// Most files kept in the folder by default.
    pub const DEFAULT_MAX_FILES: usize = 100;

    /// The default caps, for `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_file_bytes: Self::DEFAULT_MAX_FILE_BYTES,
            max_files: Self::DEFAULT_MAX_FILES,
        }
    }
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// File name inside the folder (sanitised, unique).
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Media type guessed from the extension: nothing was opened to find out.
    pub mime: String,
    /// Full path.
    pub path: PathBuf,
    /// Origin of the page that offered it, when this browser session saw it arrive.
    pub from: Option<String>,
}

/// What the folder holds and what this session knows about downloads that did not finish.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadReport {
    /// The sandboxed folder.
    pub folder: PathBuf,
    /// Finished files, newest first.
    pub finished: Vec<Download>,
    /// Names of downloads still arriving.
    pub running: Vec<String>,
    /// Downloads this session cancelled, each with the reason (the most recent ones).
    pub refused: Vec<String>,
}

/// Most refusals remembered for the report.
const MAX_REFUSED_KEPT: usize = 20;
/// Most files a report lists.
const MAX_LISTED: usize = 100;

struct Active {
    name: String,
    origin: Option<String>,
}

#[derive(Default)]
struct State {
    active: HashMap<String, Active>,
    refused: Vec<String>,
    /// Final file name -> origin that offered it (this session only).
    origins: HashMap<String, String>,
    /// Guids this module cancelled itself, so the `canceled` event is not reported twice.
    cancelled_by_us: Vec<String>,
}

/// The download state of one browser.
pub(crate) struct Downloads {
    policy: DownloadPolicy,
    state: Mutex<State>,
    /// Set when a later policy replaced this one: its listener stops.
    retired: std::sync::atomic::AtomicBool,
}

/// What to do after one browser event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `Browser.cancelDownload` for this guid, then delete any partial file.
    Cancel(String),
    /// Rename the finished file `<guid>` to `final_name`, or drop it when it is over the cap.
    Finish {
        guid: String,
        final_name: String,
        size: u64,
    },
    /// A cancelled download: remove the partial file named by the guid.
    Discard(String),
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Downloads {
    pub(crate) fn new(policy: DownloadPolicy) -> Arc<Self> {
        Arc::new(Self {
            policy,
            state: Mutex::new(State::default()),
            retired: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(crate) fn policy(&self) -> &DownloadPolicy {
        &self.policy
    }

    pub(crate) fn retire(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn is_retired(&self) -> bool {
        self.retired.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Remember a guid cancelled here (bounded: a cancel the browser never confirms is forgotten).
    fn cancelled(s: &mut State, guid: &str) {
        s.cancelled_by_us.push(guid.to_owned());
        if s.cancelled_by_us.len() > MAX_REFUSED_KEPT * 4 {
            s.cancelled_by_us.remove(0);
        }
    }

    fn refuse(s: &mut State, text: String) {
        s.refused.push(text);
        if s.refused.len() > MAX_REFUSED_KEPT {
            s.refused.remove(0);
        }
    }

    /// One `Browser.download*` event: what the caller must do about it.
    pub(crate) fn on_event(&self, method: &str, p: &Value) -> Option<Step> {
        let guid = p["guid"].as_str()?.to_owned();
        let mut s = lock(&self.state);
        match method {
            "Browser.downloadWillBegin" => {
                let name = sanitize_file_name(p["suggestedFilename"].as_str().unwrap_or_default());
                let origin = p["url"].as_str().and_then(origin_of);
                let kept = count_files(&self.policy.dir) + s.active.len();
                if kept >= self.policy.max_files {
                    let why = format!(
                        "{name}: not kept, the download folder already holds {} files (the most allowed)",
                        self.policy.max_files
                    );
                    Self::refuse(&mut s, why);
                    Self::cancelled(&mut s, &guid);
                    return Some(Step::Cancel(guid));
                }
                s.active.insert(guid, Active { name, origin });
                None
            }
            "Browser.downloadProgress" => {
                let state = p["state"].as_str().unwrap_or_default();
                let received = number(&p["receivedBytes"]);
                let total = number(&p["totalBytes"]);
                if state == "canceled" {
                    let ours = s.cancelled_by_us.iter().position(|g| *g == guid);
                    if let Some(i) = ours {
                        s.cancelled_by_us.remove(i);
                    } else if let Some(a) = s.active.remove(&guid) {
                        let why = format!("{}: the download was cancelled", a.name);
                        Self::refuse(&mut s, why);
                    }
                    s.active.remove(&guid);
                    return Some(Step::Discard(guid));
                }
                let name = s.active.get(&guid)?.name.clone();
                let cap = self.policy.max_file_bytes;
                if received > cap || total > cap {
                    s.active.remove(&guid);
                    Self::cancelled(&mut s, &guid);
                    let why = format!(
                        "{name}: not kept, it is larger than the {} MB limit",
                        cap / (1024 * 1024)
                    );
                    Self::refuse(&mut s, why);
                    return Some(Step::Cancel(guid));
                }
                if state == "completed" {
                    let a = s.active.remove(&guid)?;
                    let final_name = unique_name(&self.policy.dir, &a.name, &s.origins);
                    if let Some(origin) = a.origin {
                        s.origins.insert(final_name.clone(), origin);
                    }
                    return Some(Step::Finish {
                        guid,
                        final_name,
                        size: received,
                    });
                }
                None
            }
            _ => None,
        }
    }

    /// Execute a [`Step`] on disk and against the browser.
    pub(crate) async fn run(self: &Arc<Self>, client: &CdpClient, step: Step) {
        match step {
            Step::Cancel(guid) => {
                let _ = client
                    .send("Browser.cancelDownload", json!({"guid": guid}))
                    .await;
                let _ = tokio::fs::remove_file(self.policy.dir.join(&guid)).await;
            }
            Step::Discard(guid) => {
                let _ = tokio::fs::remove_file(self.policy.dir.join(&guid)).await;
            }
            Step::Finish {
                guid,
                final_name,
                size,
            } => {
                let from = self.policy.dir.join(&guid);
                let to = self.policy.dir.join(&final_name);
                // The size the browser reported is the file's size; check the disk as well so
                // a wrong report never lets an oversized file stay.
                let on_disk = tokio::fs::metadata(&from).await.map_or(size, |m| m.len());
                if on_disk > self.policy.max_file_bytes {
                    let _ = tokio::fs::remove_file(&from).await;
                    let why = format!(
                        "{final_name}: not kept, it is larger than the {} MB limit",
                        self.policy.max_file_bytes / (1024 * 1024)
                    );
                    Self::refuse(&mut lock(&self.state), why);
                    return;
                }
                if let Err(e) = tokio::fs::rename(&from, &to).await {
                    let why = format!("{final_name}: could not be saved ({e})");
                    Self::refuse(&mut lock(&self.state), why);
                    let _ = tokio::fs::remove_file(&from).await;
                }
            }
        }
    }

    /// Listen to the browser's download events until it closes or this policy is replaced.
    pub(crate) fn listen(self: &Arc<Self>, client: &Arc<CdpClient>) {
        let mut rx = client.subscribe();
        let weak = Arc::downgrade(client);
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if me.is_retired() {
                            break;
                        }
                        let Some(step) = me.on_event(&ev.method, &ev.params) else {
                            continue;
                        };
                        let Some(c) = weak.upgrade() else { break };
                        // Disk work runs on its own task: this loop must keep draining events.
                        let worker = Arc::clone(&me);
                        tokio::spawn(async move { worker.run(&c, step).await });
                    }
                    // Events were dropped: a finished download may now sit under its guid.
                    // `report` hides guid-named files; the sweep removes the ones no download of
                    // this session accounts for any more.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        me.sweep(ORPHAN_AGE);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Delete what a crash, a lagged event stream or a cancelled download left in the folder:
    /// a browser temp file (`*.crdownload`, `*.tmp`) or a file still under its guid that no
    /// download of this session is receiving, and that has not been touched for `min_age`.
    /// (Without this the leftovers count towards the file cap for ever.) Returns how many went.
    pub(crate) fn sweep(&self, min_age: std::time::Duration) -> usize {
        let live: std::collections::HashSet<String> =
            lock(&self.state).active.keys().cloned().collect();
        sweep_orphans(&self.policy.dir, &live, min_age)
    }

    /// Finished files in the folder plus this session's notes.
    pub(crate) fn report(&self) -> DownloadReport {
        self.sweep(ORPHAN_AGE);
        let (running, refused, origins) = {
            let s = lock(&self.state);
            (
                s.active
                    .values()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>(),
                s.refused.clone(),
                s.origins.clone(),
            )
        };
        let mut finished = list_finished(&self.policy.dir);
        for d in &mut finished {
            d.from = origins.get(&d.name).cloned();
        }
        DownloadReport {
            folder: self.policy.dir.clone(),
            finished,
            running,
            refused,
        }
    }
}

/// What a download folder holds, for a moment when no browser is running (files outlive the
/// browser that fetched them).
#[must_use]
pub fn list_folder(dir: &Path) -> DownloadReport {
    DownloadReport {
        folder: dir.to_path_buf(),
        finished: list_finished(dir),
        ..DownloadReport::default()
    }
}

fn number(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_f64().map(|f| if f > 0.0 { f as u64 } else { 0 }))
        .unwrap_or(0)
}

/// `scheme://host[:port]` of an http(s) address, for the report (no path, query or login).
fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?;
    Some(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// A browser-chosen file name: 32 hex digits with dashes (a GUID).
fn is_guid_name(name: &str) -> bool {
    name.len() == 36
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && name.matches('-').count() == 4
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .count()
    })
}

/// How long an unclaimed temp or guid file must lie untouched before it is deleted: a download
/// that just finished is renamed within moments and must never be taken for a leftover.
const ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// The same, when a browser has just started and nothing of this session can be in flight.
const ORPHAN_AGE_AT_START: std::time::Duration = std::time::Duration::from_secs(60);

/// A browser temp file or a guid-named file (see [`Downloads::sweep`]).
fn is_browser_leftover(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let temporary = Path::new(&lower)
        .extension()
        .is_some_and(|x| x == "crdownload" || x == "tmp");
    is_guid_name(name) || temporary
}

/// Delete the browser leftovers of `dir` that are not `live` guids and are older than `min_age`.
/// Only plain files (never a link or a folder) whose name is a guid or a browser temp name.
fn sweep_orphans(
    dir: &Path,
    live: &std::collections::HashSet<String>,
    min_age: std::time::Duration,
) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in rd.filter_map(std::result::Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.split('.').next().unwrap_or_default();
        if !is_browser_leftover(&name) || live.contains(&name) || live.contains(stem) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if meta.is_file() && old_enough && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn list_finished(dir: &Path) -> Vec<Download> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, Download)> = rd
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            // `symlink_metadata`: a link is not a download and is never followed.
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            if !meta.is_file() {
                return None;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if is_browser_leftover(&name) {
                return None;
            }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            Some((
                modified,
                Download {
                    mime: mime_of(&name).to_owned(),
                    size: meta.len(),
                    path: e.path(),
                    name,
                    from: None,
                },
            ))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    files.into_iter().take(MAX_LISTED).map(|(_, d)| d).collect()
}

/// Zero-width and bidirectional controls: they are not "control" characters but reorder or hide
/// what a name shows (`gpj.exe` displayed as `exe.jpg`).
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Longest file name kept, extension included.
const MAX_NAME_CHARS: usize = 120;

/// Types a double click would run or hand to a script host.
const LAUNCHABLE: &[&str] = &[
    "exe", "msi", "bat", "cmd", "com", "scr", "pif", "ps1", "psm1", "vbs", "vbe", "js", "jse",
    "wsf", "wsh", "hta", "jar", "lnk", "dll", "reg", "cpl", "sh", "command", "app", "apk",
    "appimage", "desktop", "scf", "url", "inf", "gadget", "ps1xml", "psd1", "msc", "jnlp",
    // A page the browser renders, with its scripts, when opened by a double click: a saved
    // `.html` or `.svg` runs script in the `file:` origin of the owner's computer.
    "html", "htm", "xhtml", "xht", "shtml", "svg", "svgz", "mhtml", "mht", "mhtm",
];

const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A plain file name for what the page suggested: no folders, no control characters, nothing a
/// file system or a shell treats specially, and a `.download` suffix on a launchable type.
#[must_use]
pub fn sanitize_file_name(raw: &str) -> String {
    // Only the last component: `..\..\x.txt` and `/etc/passwd` both lose their folders.
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = last
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    name = name
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned();
    if name.is_empty() {
        "download".clone_into(&mut name);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_owned(), Some(ext.to_owned())),
        _ => (name.clone(), None),
    };
    let stem = if WINDOWS_RESERVED.contains(&stem.to_ascii_lowercase().as_str()) {
        format!("_{stem}")
    } else {
        stem
    };
    let ext = ext.map(|e| e.trim_end_matches([' ', '.']).to_owned());
    let mut out = match &ext {
        Some(e) if !e.is_empty() => format!("{stem}.{e}"),
        _ => stem,
    };
    if out.chars().count() > MAX_NAME_CHARS {
        let keep_ext = ext
            .as_deref()
            .filter(|e| e.chars().count() <= 12)
            .map(str::to_owned);
        let budget = MAX_NAME_CHARS - keep_ext.as_ref().map_or(0, |e| e.chars().count() + 1);
        let stem: String = out.chars().take(budget).collect();
        out = match keep_ext {
            Some(e) => format!("{stem}.{e}"),
            None => stem,
        };
    }
    let is_launchable = out
        .rsplit_once('.')
        .is_some_and(|(_, e)| LAUNCHABLE.contains(&e.to_ascii_lowercase().as_str()));
    if is_launchable {
        out.push_str(".download");
    }
    out
}

/// `name`, or `stem (2).ext`, `stem (3).ext`... when the folder has it (or this session
/// already promised it).
fn unique_name(dir: &Path, name: &str, taken: &HashMap<String, String>) -> String {
    let free = |n: &str| !dir.join(n).exists() && !taken.contains_key(n);
    if free(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    for n in 2..1000 {
        let candidate = format!("{stem} ({n}){ext}");
        if free(&candidate) {
            return candidate;
        }
    }
    format!(
        "{stem} ({}){ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    )
}

/// A media type by file extension; `application/octet-stream` when unknown.
#[must_use]
pub fn mime_of(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    // A `.download` suffix hides the real type; look under it.
    if ext == "download" {
        return "application/octet-stream";
    }
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
}

/// The plain form of a verbatim path (`\\?\C:\x` -> `C:\x`, `\\?\UNC\h\s` -> `\\h\s`), which
/// is what the browser is given: it does not take the verbatim prefix `canonicalize` adds on
/// Windows. A path that is not a plain drive or UNC form is returned unchanged (a prefix is never
/// cut blindly, `\\?\UNC\h\s` would turn into the relative `UNC\h\s`). A no-op off Windows.
#[must_use]
pub fn plain_path(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let mut chars = rest.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.next() == Some(':')
            && matches!(chars.next(), Some('\\') | None)
        {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// `policy` with its folder resolved to the plain absolute path the browser is given (see
/// [`plain_path`]). A folder that does not exist yet is left as written.
pub(crate) fn resolved(policy: &DownloadPolicy) -> DownloadPolicy {
    let dir = std::fs::canonicalize(&policy.dir).unwrap_or_else(|_| policy.dir.clone());
    DownloadPolicy {
        dir: plain_path(&dir),
        ..policy.clone()
    }
}

/// Create the folder and tell the browser to put downloads there under random names.
pub(crate) async fn enable(
    client: &Arc<CdpClient>,
    policy: DownloadPolicy,
) -> Result<Arc<Downloads>> {
    tokio::fs::create_dir_all(&policy.dir)
        .await
        .map_err(|e| Error::Io(format!("download folder {}: {e}", policy.dir.display())))?;
    let policy = resolved(&policy);
    let path = policy.dir.to_string_lossy().into_owned();
    let downloads = Downloads::new(policy);
    // A crashed earlier browser may have left partial files that would count against the cap.
    downloads.sweep(ORPHAN_AGE_AT_START);
    // Listen first: an event sent right after the command must not be missed.
    downloads.listen(client);
    client
        .send(
            "Browser.setDownloadBehavior",
            json!({"behavior": "allowAndName", "downloadPath": path, "eventsEnabled": true}),
        )
        .await?;
    Ok(downloads)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> DownloadPolicy {
        DownloadPolicy {
            dir: dir.to_path_buf(),
            max_file_bytes: 1000,
            max_files: 3,
        }
    }

    fn begin(d: &Downloads, guid: &str, name: &str) -> Option<Step> {
        d.on_event(
            "Browser.downloadWillBegin",
            &json!({"guid": guid, "url": "https://files.example.com/a/b?token=secret", "suggestedFilename": name}),
        )
    }

    fn progress(d: &Downloads, guid: &str, state: &str, received: u64, total: u64) -> Option<Step> {
        d.on_event(
            "Browser.downloadProgress",
            &json!({"guid": guid, "state": state, "receivedBytes": received, "totalBytes": total}),
        )
    }

    #[test]
    fn a_suggested_name_never_names_a_folder_or_a_special_file() {
        for (raw, want) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            (r"..\..\Windows\win.ini", "win.ini"),
            ("C:\\x\\y.txt", "y.txt"),
            ("a<b>c:d\"e|f?g*h.txt", "a_b_c_d_e_f_g_h.txt"),
            ("  ..hidden  ", "hidden"),
            ("trailing.dot.", "trailing.dot"),
            ("", "download"),
            ("///", "download"),
            ("CON.txt", "_CON.txt"),
            ("nul", "_nul"),
            ("lpt1.log", "_lpt1.log"),
            ("evil\u{202e}gpj.exe", "evilgpj.exe.download"),
            ("tab\tname\n.txt", "tabname.txt"),
        ] {
            assert_eq!(sanitize_file_name(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn a_launchable_type_gets_a_download_suffix_and_a_long_name_keeps_its_extension() {
        for raw in [
            "setup.exe",
            "run.BAT",
            "x.ps1",
            "a.js",
            "x.lnk",
            "tool.jar",
            "run.sh",
            "p.msi",
        ] {
            assert!(sanitize_file_name(raw).ends_with(".download"), "{raw}");
        }
        assert_eq!(sanitize_file_name("notes.txt"), "notes.txt");
        let long = format!("{}.pdf", "a".repeat(400));
        let n = sanitize_file_name(&long);
        assert_eq!(n.chars().count(), MAX_NAME_CHARS);
        assert!(Path::new(&n).extension().is_some_and(|x| x == "pdf"));
    }

    #[test]
    fn a_verbatim_path_becomes_a_plain_one_only_in_a_known_form() {
        let plain = |s: &str| plain_path(Path::new(s)).to_string_lossy().into_owned();
        assert_eq!(plain(r"\\?\C:\Users\a\dl"), r"C:\Users\a\dl");
        assert_eq!(plain(r"\\?\UNC\host\share\dl"), r"\\host\share\dl");
        // not a drive and not UNC: left alone, never cut into a relative path
        assert_eq!(plain(r"\\?\Volume{1234}\dl"), r"\\?\Volume{1234}\dl");
        assert_eq!(plain(r"\\?\GLOBALROOT\x"), r"\\?\GLOBALROOT\x");
        assert_eq!(plain("/home/a/dl"), "/home/a/dl");
        assert_eq!(plain(r"C:\x"), r"C:\x");
    }

    #[test]
    fn media_type_comes_from_the_extension_only() {
        assert_eq!(mime_of("a.PDF"), "application/pdf");
        assert_eq!(mime_of("a.csv"), "text/csv");
        assert_eq!(mime_of("noext"), "application/octet-stream");
        assert_eq!(mime_of("weird.zzz"), "application/octet-stream");
        assert_eq!(mime_of("setup.exe.download"), "application/octet-stream");
    }

    #[test]
    fn a_download_over_the_cap_is_cancelled_while_it_arrives_and_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let d = Downloads::new(policy(dir.path()));
        assert_eq!(begin(&d, "g1", "big.bin"), None);
        assert_eq!(
            progress(&d, "g1", "inProgress", 500, 5000),
            Some(Step::Cancel("g1".into()))
        );
        // The browser confirms the cancellation: nothing is reported a second time.
        assert_eq!(
            progress(&d, "g1", "canceled", 500, 5000),
            Some(Step::Discard("g1".into()))
        );
        let r = d.report();
        assert_eq!(r.refused.len(), 1, "{:?}", r.refused);
        assert!(r.refused[0].contains("big.bin") && r.refused[0].contains("limit"));
        assert!(r.running.is_empty() && r.finished.is_empty());
    }

    #[test]
    fn a_download_that_grows_past_the_cap_without_a_declared_size_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let d = Downloads::new(policy(dir.path()));
        begin(&d, "g1", "stream.bin");
        assert_eq!(progress(&d, "g1", "inProgress", 400, 0), None);
        assert_eq!(
            progress(&d, "g1", "inProgress", 1500, 0),
            Some(Step::Cancel("g1".into()))
        );
    }

    #[test]
    fn a_finished_download_is_renamed_to_a_unique_sanitised_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let d = Downloads::new(policy(dir.path()));
        begin(&d, "g1", "../a.txt");
        let step = progress(&d, "g1", "completed", 10, 10);
        assert_eq!(
            step,
            Some(Step::Finish {
                guid: "g1".into(),
                final_name: "a (2).txt".into(),
                size: 10
            })
        );
        // A second file with the same name in the same breath does not collide either.
        begin(&d, "g2", "a.txt");
        std::fs::write(dir.path().join("a (2).txt"), b"x").unwrap();
        let Some(Step::Finish { final_name, .. }) = progress(&d, "g2", "completed", 3, 3) else {
            panic!("finish expected");
        };
        assert_eq!(final_name, "a (3).txt");
    }

    #[test]
    fn the_file_count_cap_cancels_new_downloads() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..3 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), b"x").unwrap();
        }
        let d = Downloads::new(policy(dir.path()));
        assert_eq!(
            begin(&d, "g1", "one-too-many.txt"),
            Some(Step::Cancel("g1".into()))
        );
        assert!(d.report().refused[0].contains("3 files"));
    }

    #[test]
    fn the_report_lists_finished_files_newest_first_without_browser_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("older.txt"), b"1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(dir.path().join("newer.pdf"), b"123").unwrap();
        std::fs::write(
            dir.path().join("0b8a5c1e-aaaa-bbbb-cccc-0123456789ab"),
            b"partial",
        )
        .unwrap();
        std::fs::write(dir.path().join("x.crdownload"), b"p").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let d = Downloads::new(DownloadPolicy {
            max_files: 50,
            ..policy(dir.path())
        });
        begin(&d, "g9", "arriving.zip");
        let r = d.report();
        let names: Vec<_> = r.finished.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["newer.pdf", "older.txt"]);
        assert_eq!(r.finished[0].size, 3);
        assert_eq!(r.finished[0].mime, "application/pdf");
        assert_eq!(r.running, ["arriving.zip"]);
    }

    #[test]
    fn a_page_a_browser_renders_gets_a_download_suffix_too() {
        for raw in [
            "report.html",
            "a.HTM",
            "pic.svg",
            "saved.mhtml",
            "x.xhtml",
            "launch.jnlp",
            "snap.msc",
            "m.ps1xml",
            "w.gadget",
        ] {
            assert!(sanitize_file_name(raw).ends_with(".download"), "{raw}");
        }
        // Read through the suffix the type is unknown, so nothing opens it by mistake.
        assert_eq!(mime_of("report.html.download"), "application/octet-stream");
        assert_eq!(sanitize_file_name("notes.md"), "notes.md");
    }

    fn age(path: &Path, secs: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn leftovers_nobody_is_receiving_are_swept_and_do_not_count_against_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let (orphan, live, fresh, partial) = (
            "0b8a5c1e-aaaa-bbbb-cccc-0123456789ab",
            "1b8a5c1e-aaaa-bbbb-cccc-0123456789ab",
            "2b8a5c1e-aaaa-bbbb-cccc-0123456789ab",
            "partial.crdownload",
        );
        for n in [orphan, live, fresh, partial, "keep.txt"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        for n in [orphan, live, partial, "keep.txt"] {
            age(&dir.path().join(n), 3600);
        }
        let d = Downloads::new(DownloadPolicy {
            max_files: 50,
            ..policy(dir.path())
        });
        // `live` is being received by this session; `fresh` was just written
        assert_eq!(begin(&d, live, "arriving.bin"), None);
        assert_eq!(d.sweep(ORPHAN_AGE), 2);
        assert!(!dir.path().join(orphan).exists());
        assert!(!dir.path().join(partial).exists());
        for n in [live, fresh, "keep.txt"] {
            assert!(dir.path().join(n).exists(), "{n} must stay");
        }
        // a folder named like a leftover is never touched; without an age limit `fresh` goes
        let folder = dir.path().join("3b8a5c1e-aaaa-bbbb-cccc-0123456789ab");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(d.sweep(std::time::Duration::ZERO), 1);
        assert!(folder.is_dir());
        assert!(dir.path().join(live).exists());
    }

    #[test]
    fn the_report_sweeps_old_leftovers_before_counting() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..3 {
            let p = dir
                .path()
                .join(format!("{n}b8a5c1e-aaaa-bbbb-cccc-0123456789ab"));
            std::fs::write(&p, b"x").unwrap();
            age(&p, 3600);
        }
        let d = Downloads::new(policy(dir.path()));
        let _ = d.report();
        // the cap of 3 files is not used up by crash leftovers any more
        assert_eq!(begin(&d, "g1", "fresh.txt"), None);
    }

    #[test]
    fn the_source_is_an_origin_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = Downloads::new(policy(dir.path()));
        begin(&d, "g1", "a.txt");
        let Some(Step::Finish { final_name, .. }) = progress(&d, "g1", "completed", 1, 1) else {
            panic!("finish expected");
        };
        std::fs::write(dir.path().join(&final_name), b"x").unwrap();
        let r = d.report();
        assert_eq!(
            r.finished[0].from.as_deref(),
            Some("https://files.example.com")
        );
        assert_eq!(origin_of("file:///c:/x"), None);
        assert_eq!(
            origin_of("http://h:8080/p?q=1").as_deref(),
            Some("http://h:8080")
        );
    }
}
