// Adapted from vercel-labs/agent-browser (Apache-2.0): browser discovery and launch flags.
//! Locate Chrome/Edge, build launch arguments, and resolve a CDP websocket endpoint.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// Environment variable that overrides browser discovery.
pub const BROWSER_ENV: &str = "PRAXI_BROWSER_PATH";

/// How to launch a browser.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Browser binary; discovered with [`find_browser`] when `None`.
    pub executable: Option<PathBuf>,
    /// Run without a window (`--headless=new`).
    pub headless: bool,
    /// Profile directory. `None` creates an isolated temporary profile that is deleted on close.
    pub user_data_dir: Option<PathBuf>,
    /// Extra command line flags.
    pub args: Vec<String>,
    /// Initial window size.
    pub window_size: (u32, u32),
    /// How long to wait for the debugging endpoint to appear.
    pub startup_timeout: Duration,
    /// Network guard of a served session (a filtering proxy for every
    /// request, children paused until checked); `None`: no network policy.
    pub served: Option<crate::destination::ServedNetwork>,
    /// The guard's proxy address, set by [`crate::Browser::launch`] when
    /// `served` is set (never by callers).
    pub served_proxy: Option<std::net::SocketAddr>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            executable: None,
            headless: true,
            user_data_dir: None,
            args: Vec::new(),
            window_size: (1280, 800),
            startup_timeout: Duration::from_secs(20),
            served: None,
            served_proxy: None,
        }
    }
}

/// Subfolder of the data directory that holds a browser Praxi downloaded itself.
pub const MANAGED_DIR: &str = "browser";

/// File an install writes last (the folder's checksum manifest): a half-written
/// install never has it, so it is never picked.
pub const MANAGED_MARKER: &str = "praxi-runtime.manifest";

/// Checks that an install folder still matches the checksum manifest it was written with.
/// Given the program (`<root>/i-*/<dir>/chrome`); `true` only when every file of the install
/// folder is exactly what the install recorded. The checksum code lives in a crate this one does
/// not depend on, so the daemon hands it over at start-up.
pub type ManifestVerifier = fn(&Path) -> bool;

static MANAGED_ROOT: std::sync::OnceLock<(PathBuf, ManifestVerifier)> = std::sync::OnceLock::new();

/// Tell discovery where the data directory is, so a browser installed by
/// `bot browser install` (or the Settings button) is found like any other, and how to check
/// its manifest. Without this call (or with a verifier that says no) a managed browser is
/// never used: a marker file and an executable beside it prove nothing.
/// The first call wins; call it once at start-up.
pub fn register_managed(data_dir: &Path, verify: ManifestVerifier) {
    let _ = MANAGED_ROOT.set((data_dir.join(MANAGED_DIR), verify));
}

/// What one candidate path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// A regular file that is not a snap wrapper.
    Usable,
    /// A snap browser or a wrapper that runs one (cannot run in the hardened unit).
    Snap,
    /// Writable by every user (the file, or the folder it really lives in): anyone on the
    /// computer could swap it for their own program.
    Unsafe,
    /// Nothing there.
    Missing,
}

fn probe_path(p: &Path) -> Probe {
    if !p.is_file() {
        Probe::Missing
    } else if writable_by_everyone(p) {
        Probe::Unsafe
    } else if is_snap_browser(p) {
        Probe::Snap
    } else {
        Probe::Usable
    }
}

/// Whether the program `path` really is (links followed), or the folder it really lives in,
/// can be written by every user. Unknown counts as yes: a path that cannot be resolved cannot
/// be vouched for.
fn writable_by_everyone(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(real) = path.canonicalize() else {
            return true;
        };
        let other_writes = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.mode() & 0o002 != 0);
        other_writes(&real) || real.parent().is_none_or(other_writes)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// What to do when no browser can be used, in plain words.
#[must_use]
pub(crate) fn install_advice() -> &'static str {
    if cfg!(target_os = "linux") {
        "Install one with `bot browser install` (or Settings > Diagnostics > Install browser), or install Google Chrome or a non-snap Chromium yourself, or set BOT_BROWSER_PATH to its path"
    } else {
        "Install Google Chrome or Microsoft Edge, or set BOT_BROWSER_PATH to its path"
    }
}

/// Pick the browser: an explicit override wins (and must work, or the error
/// says so); otherwise the first usable candidate. Snap browsers are never
/// picked, and when only those exist the error names them. Pure: the file
/// system is behind `probe`.
///
/// # Errors
/// [`Error::BrowserNotFound`] when nothing exists, [`Error::Launch`] for a bad
/// override or when only snap browsers exist.
pub fn select_browser(
    override_path: Option<&Path>,
    candidates: &[PathBuf],
    probe: &dyn Fn(&Path) -> Probe,
) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return match probe(p) {
            Probe::Usable => Ok(p.to_owned()),
            Probe::Snap => Err(Error::Launch(format!(
                "BOT_BROWSER_PATH points to a snap browser ({}), which cannot run under the hardened service. {}",
                p.display(),
                install_advice()
            ))),
            Probe::Unsafe => Err(Error::Launch(format!(
                "BOT_BROWSER_PATH points to {} (really {}), which every user on this computer can write to, either the file or its folder. Remove the write permission for others (chmod o-w) or point it at another browser",
                p.display(),
                p.canonicalize().as_deref().unwrap_or(p).display()
            ))),
            Probe::Missing => Err(Error::Launch(format!(
                "BOT_BROWSER_PATH points to {}, which does not exist. Fix the path or unset it",
                p.display()
            ))),
        };
    }
    let mut snap = None;
    for c in candidates {
        match probe(c) {
            Probe::Usable => return Ok(c.clone()),
            Probe::Snap => {
                snap.get_or_insert(c);
            }
            // Never picked, never an error of its own: the next candidate gets its turn.
            Probe::Unsafe | Probe::Missing => {}
        }
    }
    Err(match snap {
        Some(s) => Error::Launch(format!(
            "the only browser found is a snap package ({}), which cannot run under the hardened service. {}",
            s.display(),
            install_advice()
        )),
        None => Error::BrowserNotFound,
    })
}

/// Find an installed Chrome or Edge binary, for a launch: a browser Praxi downloaded itself is
/// used only after every file of it was hashed against its manifest just now.
///
/// # Errors
/// See [`select_browser`].
pub fn find_browser() -> Result<PathBuf> {
    find_browser_with(true)
}

/// [`find_browser`] for callers that only report or hand on the path (status, doctor, the
/// shell's environment): the manifest check of a downloaded browser is remembered for a minute,
/// because it hashes the whole install. The launch itself always uses [`find_browser`].
///
/// # Errors
/// See [`select_browser`].
pub fn find_browser_cached() -> Result<PathBuf> {
    find_browser_with(false)
}

fn find_browser_with(fresh: bool) -> Result<PathBuf> {
    let over = std::env::var("BOT_BROWSER_PATH")
        .or_else(|_| std::env::var(BROWSER_ENV))
        .ok()
        .filter(|p| !p.trim().is_empty());
    let found = select_browser(
        over.as_deref().map(Path::new),
        &candidate_paths(),
        &probe_path,
    );
    let found = match found {
        // Last resort, and only here: hashing a whole install is not free.
        Err(e) if over.is_none() => managed_candidate(fresh)
            .filter(|b| probe_path(b) == Probe::Usable)
            .ok_or(e),
        other => other,
    };
    // The override is the owner's own setting and is honoured as written; the log says what it
    // really resolved to (a link can point anywhere).
    if let Some(o) = &over {
        match &found {
            Ok(p) => tracing::info!(
                configured = %o,
                resolved = %p.canonicalize().as_deref().unwrap_or(p).display(),
                "using the browser named by BOT_BROWSER_PATH"
            ),
            Err(e) => tracing::warn!(configured = %o, "BOT_BROWSER_PATH refused: {e}"),
        }
    }
    found
}

/// How long [`find_browser_cached`] trusts a manifest check.
const MANAGED_CHECK_TTL: Duration = Duration::from_secs(60);

type ManagedCheck = Option<(std::time::Instant, Option<PathBuf>)>;

static MANAGED_CHECK: std::sync::Mutex<ManagedCheck> = std::sync::Mutex::new(None);

/// Forget the remembered manifest check (after an install changed what is on disk).
pub fn forget_managed_check() {
    *MANAGED_CHECK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

fn managed_candidate(fresh: bool) -> Option<PathBuf> {
    let (root, verify) = MANAGED_ROOT.get()?;
    let mut last = MANAGED_CHECK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !fresh
        && let Some((at, found)) = last.as_ref()
        && at.elapsed() < MANAGED_CHECK_TTL
    {
        return found.clone();
    }
    let found = managed_browser(root, verify);
    *last = Some((std::time::Instant::now(), found.clone()));
    found
}

/// Known snap executables and distro wrappers cannot run in the hardened unit.
#[must_use]
pub fn is_snap_browser(path: &Path) -> bool {
    use std::io::Read;
    if path.starts_with("/snap") || path.canonicalize().is_ok_and(|p| p.starts_with("/snap")) {
        return true;
    }
    let mut header = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(8192).read_to_end(&mut header);
    }
    header.starts_with(b"#!")
        && (String::from_utf8_lossy(&header).contains("snap run")
            || String::from_utf8_lossy(&header).contains("/snap/"))
}

/// The newest install under `root` (`<data dir>/browser`) that passes `verify`: a folder with
/// the install marker and a regular `chrome` file one level down, whose files all still match the
/// manifest. The marker's existence alone never counts, so a planted folder is never used.
/// `root` itself must belong to this user (or root) and not be writable by anyone else.
#[must_use]
pub fn managed_browser(root: &Path, verify: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    if !dir_is_private(root) {
        return None;
    }
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in std::fs::read_dir(root).ok()?.flatten() {
        // A link in place of an install folder is never followed.
        if !dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = dir.path();
        let Ok(done) = std::fs::metadata(dir.join(MANAGED_MARKER)).and_then(|m| m.modified())
        else {
            continue;
        };
        let Ok(inner) = std::fs::read_dir(&dir) else {
            continue;
        };
        for sub in inner.flatten() {
            let bin = sub.path().join("chrome");
            if std::fs::symlink_metadata(&bin).is_ok_and(|m| m.is_file()) {
                found.push((done, bin));
            }
        }
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, b)| b).find(|b| verify(b))
}

/// A folder that only this user or root can change: owned by one of them, and with no write
/// bit for group or others. (Always true where there are no such permissions.)
fn dir_is_private(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir)
            .is_ok_and(|m| m.is_dir() && owner_and_mode_trusted(m.uid(), m.mode(), service_uid()))
    }
    #[cfg(not(unix))]
    {
        dir.is_dir()
    }
}

/// The pure rule behind [`dir_is_private`]: owned by root or the service user, and neither
/// group nor others can write.
#[cfg(unix)]
fn owner_and_mode_trusted(uid: u32, mode: u32, service_uid: u32) -> bool {
    (uid == 0 || uid == service_uid) && mode & 0o022 == 0
}

#[cfg(unix)]
fn service_uid() -> u32 {
    #[cfg(target_os = "linux")]
    {
        rustix::process::geteuid().as_raw()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Not reachable where only Linux enumerates folders; unknown matches only root.
        u32::MAX
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        let roots: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
            .collect();
        for r in &roots {
            v.push(r.join(r"Google\Chrome\Application\chrome.exe"));
        }
        for r in &roots {
            v.push(r.join(r"Microsoft\Edge\Application\msedge.exe"));
        }
    } else if cfg!(target_os = "macos") {
        v.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        v.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
    } else {
        v.push("/opt/praxi-browser/chrome".into());
        v.extend(linux_candidates(std::env::var_os("PATH").as_deref()));
        // The browser Praxi downloaded comes after all of these, and only once its manifest
        // checks out: see `find_browser`.
    }
    v
}

/// System-installed browsers on Linux: the fixed distro places, then the `PATH` folders that
/// only this user or root can change (a drop-in `chromium` in a folder anyone can write to would
/// run as the service), then the programs behind the distro launcher scripts.
fn linux_candidates(path_env: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "microsoft-edge-stable",
    ];
    let fixed: Vec<PathBuf> = [
        "/usr/bin",
        "/usr/local/bin",
        "/opt/google/chrome",
        "/snap/bin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let mut v: Vec<PathBuf> = Vec::new();
    for d in &fixed {
        v.extend(names.iter().map(|n| d.join(n)));
    }
    if let Some(p) = path_env {
        for d in std::env::split_paths(p).filter(|d| d.is_absolute() && !fixed.contains(d)) {
            if dir_is_private(&d) {
                v.extend(
                    names
                        .iter()
                        .map(|n| d.join(n))
                        .filter(|c| stays_inside(c, &d)),
                );
            }
        }
    }
    // The programs behind distro launcher scripts, for hosts that only have these.
    v.extend(
        [
            "/opt/google/chrome/chrome",
            "/usr/lib/chromium/chromium",
            "/usr/lib64/chromium-browser/chromium-browser",
        ]
        .map(PathBuf::from),
    );
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| seen.insert(p.clone()));
    v
}

/// Whether `candidate` is not a link, or is one whose target is still inside `dir`: a link in a
/// `PATH` folder that leads somewhere else is not what the folder vouches for.
fn stays_inside(candidate: &Path, dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(candidate) else {
        // Nothing there: the probe will say so.
        return true;
    };
    !meta.is_symlink()
        || matches!(
            (candidate.canonicalize(), dir.canonicalize()),
            (Ok(real), Ok(base)) if real.starts_with(&base)
        )
}

/// Command line flags for a launch (excluding the binary).
pub fn build_args(opts: &LaunchOptions, profile: &Path) -> Vec<String> {
    let linux = cfg!(target_os = "linux");
    let display = crate::host::has_display();
    let screen = if fills_screen(opts, linux, display) {
        crate::host::screen_size()
    } else {
        None
    };
    let mut args = build_args_for_host(opts, profile, linux, display, running_as_root(), screen);
    if linux {
        insert_before_url(
            &mut args,
            language_args(crate::host::host_languages(), &opts.args),
        );
    }
    args
}

/// `--lang` and `--accept-lang` for the host's language, so pages see what the
/// owner of the host would (a service-started browser says `en-US` otherwise).
/// Nothing when the host has none, when the caller already chose one, or when
/// the text is not a plain language list (`ll`, `ll-RR`, comma separated): it
/// goes onto a command line, so it is checked here and not by the caller.
#[must_use]
pub fn language_args(languages: Option<String>, extra: &[String]) -> Vec<String> {
    let chosen = extra
        .iter()
        .any(|a| a.starts_with("--lang") || a.starts_with("--accept-lang"));
    match languages {
        Some(l) if !chosen && is_language_list(&l) => {
            let first = l.split(',').next().unwrap_or(&l).to_owned();
            vec![format!("--lang={first}"), format!("--accept-lang={l}")]
        }
        _ => Vec::new(),
    }
}

/// One to four tags of the form `ll` or `ll-RR` (two or three lower-case letters, then an
/// optional dash and two upper-case letters), separated by commas, nothing else.
fn is_language_list(text: &str) -> bool {
    let tag_ok = |t: &str| {
        let (lang, region) = match t.split_once('-') {
            Some((l, r)) => (l, Some(r)),
            None => (t, None),
        };
        (2..=3).contains(&lang.len())
            && lang.bytes().all(|b| b.is_ascii_lowercase())
            && region.is_none_or(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase()))
    };
    let mut tags = 0;
    for t in text.split(',') {
        tags += 1;
        if tags > 4 || !tag_ok(t) {
            return false;
        }
    }
    true
}

/// Browser switches nobody who can influence a launch may add: they switch off the sandbox or
/// site isolation or certificate checks, open the debugging port to others, point the browser at
/// another profile, or run a program of the caller's choice. Compared by name, without leading
/// dashes, case-insensitively; a trailing `*` means "starts with".
const DENIED_SWITCHES: &[&str] = &[
    "no-sandbox",
    "disable-setuid-sandbox",
    "disable-gpu-sandbox",
    "disable-web-security",
    "disable-site-isolation-trials",
    "allow-running-insecure-content",
    "allow-file-access-from-files",
    "unsafely-treat-insecure-origin-as-secure",
    "ignore-certificate-errors*",
    "remote-debugging-*",
    "remote-allow-origins",
    "user-data-dir",
    "load-extension",
    "disable-extensions-except",
    "renderer-cmd-prefix",
    "utility-cmd-prefix",
    "gpu-launcher",
    "browser-subprocess-path",
];

/// Whether an extra launch argument is refused: a denied switch, or anything that is not a
/// switch at all (a bare word is a page for the browser to open).
#[must_use]
pub fn extra_arg_denied(arg: &str) -> bool {
    let arg = arg.trim();
    let Some(rest) = arg.strip_prefix('-') else {
        return true;
    };
    let name = rest
        .trim_start_matches('-')
        .split('=')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    DENIED_SWITCHES.iter().any(|d| match d.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == *d,
    })
}

/// Refuse a launch whose extra arguments contain a denied one. The sandbox is only ever turned
/// off by this module's own decision (the browser runs as root), never by a caller.
///
/// # Errors
/// [`Error::Launch`] naming the first refused argument.
pub fn check_extra_args(args: &[String]) -> Result<()> {
    match args.iter().find(|a| extra_arg_denied(a)) {
        Some(a) => Err(Error::Launch(format!(
            "the browser flag `{}` is not allowed: it weakens the browser's isolation or changes what it runs",
            a.split('=').next().unwrap_or(a)
        ))),
        None => Ok(()),
    }
}

/// Flags go before the page the browser opens (always the last argument).
fn insert_before_url(args: &mut Vec<String>, flags: Vec<String>) {
    let at = args.len().saturating_sub(1);
    args.splice(at..at, flags);
}

/// Whether the browser runs without a window on this host.
#[must_use]
pub fn runs_headless(opts: &LaunchOptions, linux: bool, display: bool) -> bool {
    opts.headless || (linux && !display)
}

/// A headed browser on a Linux X display is the bot's own computer: its
/// window takes the whole virtual screen instead of sitting small in a corner.
/// (A headed browser on a person's own PC keeps the size it asked for.)
#[must_use]
pub fn fills_screen(opts: &LaunchOptions, linux: bool, display: bool) -> bool {
    linux && display && !runs_headless(opts, linux, display)
}

fn running_as_root() -> bool {
    #[cfg(target_os = "linux")]
    {
        rustix::process::geteuid().is_root()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Pure policy entry point for server launch tests. Extra flags cannot disable the sandbox.
pub fn build_args_for_host(
    opts: &LaunchOptions,
    profile: &Path,
    linux: bool,
    display: bool,
    root: bool,
    screen: Option<(u32, u32)>,
) -> Vec<String> {
    // The window covers the virtual screen when it is known; otherwise the
    // requested size stays (and `--start-maximized` still asks the window
    // manager, and the CDP call after launch asks the browser, to maximise).
    let fill = fills_screen(opts, linux, display);
    let (win_w, win_h) = match screen {
        Some(size) if fill && size.0 > 0 && size.1 > 0 => size,
        _ => opts.window_size,
    };
    let mut a = vec![
        "--remote-debugging-port=0".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-default-apps".into(),
        "--disable-sync".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-gpu".into(),
        format!("--window-size={win_w},{win_h}"),
        // The managed profile belongs to the daemon and is often stopped
        // abruptly: never ask the person watching to "Restore pages", and
        // never open an error dialog nobody can click.
        "--hide-crash-restore-bubble".into(),
        "--disable-session-crashed-bubble".into(),
        "--noerrdialogs".into(),
        // Chrome for Testing shows an "only for automated testing" bar above
        // the page; this switch hides non-interactive info bars only. It
        // touches neither the sandbox nor site isolation.
        "--disable-infobars".into(),
    ];
    if fill {
        a.push("--start-maximized".into());
        a.push("--window-position=0,0".into());
    }
    // Disposable test profiles need no keyring. Persistent profiles must retain
    // Chromium's native OS cookie encryption instead of forcing plaintext storage.
    if opts.user_data_dir.is_none() {
        a.extend([
            "--password-store=basic".into(),
            "--use-mock-keychain".into(),
        ]);
    }
    let served = opts.served.is_some() || opts.served_proxy.is_some();
    if !served {
        // Our own sessions open popups as tabs; a served one keeps Chrome's
        // popup blocker.
        a.push("--disable-popup-blocking".into());
    }
    if let Some(proxy) = opts.served_proxy {
        a.extend(crate::proxy::FilterProxy::chrome_args(proxy));
    }
    if runs_headless(opts, linux, display) {
        a.push("--headless=new".into());
    }
    a.extend(
        opts.args
            .iter()
            // Defence in depth: `Browser::launch` refuses these with an error first.
            .filter(|arg| !extra_arg_denied(arg))
            .filter(|arg| {
                !linux
                    || (!arg.starts_with("--no-sandbox")
                        && !arg.starts_with("--disable-setuid-sandbox")
                        && !arg.starts_with("--headless"))
            })
            // Extra flags never loosen a served session's network guard.
            .filter(|arg| {
                !served
                    || !(arg.starts_with("--proxy")
                        || arg.starts_with("--host-resolver-rules")
                        || arg.starts_with("--no-proxy")
                        || arg.starts_with("--disable-popup-blocking")
                        || arg.starts_with("--force-webrtc-ip-handling-policy"))
            })
            .cloned(),
    );
    if linux && root {
        let warning = "Chromium is running as root; --no-sandbox is required. Run the host as an unprivileged user to keep the browser sandbox";
        // The CLI may have no tracing subscriber; systemd still captures stderr.
        if !tracing::enabled!(tracing::Level::WARN) {
            eprintln!("warning: {warning}");
        }
        tracing::warn!("{warning}");
        a.push("--no-sandbox".into());
    }
    a.push("about:blank".into());
    a
}

/// End of the browser's stderr kept for a launch error (bytes).
pub(crate) const STDERR_KEEP: usize = 2048;
/// Start of it kept too (bytes): the cause is usually the first line, and a
/// crash dump after it would push it out of the end.
pub(crate) const STDERR_HEAD: usize = 1024;

/// The first and the last part of what a browser wrote to stderr.
#[derive(Debug, Default)]
pub(crate) struct StderrKeep {
    head: Vec<u8>,
    tail: Vec<u8>,
    cut: bool,
}

impl StderrKeep {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let room = STDERR_HEAD.saturating_sub(self.head.len()).min(bytes.len());
        let (first, rest) = bytes.split_at(room);
        self.head.extend_from_slice(first);
        self.tail.extend_from_slice(rest);
        if self.tail.len() > STDERR_KEEP {
            let cut = self.tail.len() - STDERR_KEEP;
            self.tail.drain(..cut);
            self.cut = true;
        }
    }

    pub(crate) fn text(&self) -> String {
        let mut all = self.head.clone();
        if self.cut {
            all.extend_from_slice(b"\n[...]\n");
        }
        all.extend_from_slice(&self.tail);
        String::from_utf8_lossy(&all).into_owned()
    }
}

/// Plain-language cause for the stderr text of a browser that did not start,
/// or `None` when nothing recognisable is in it.
#[must_use]
pub fn launch_hint(stderr: &str) -> Option<&'static str> {
    let t = stderr.to_ascii_lowercase();
    if t.contains("missing x server")
        || t.contains("cannot open display")
        || t.contains("unable to open x display")
    {
        Some(
            "the X display is not reachable from the daemon (DISPLAY unset, X not running, or XAUTHORITY not readable)",
        )
    } else if t.contains("authorization required") || t.contains("xauthority") {
        Some("the X display refused the daemon (XAUTHORITY missing or wrong)")
    } else if t.contains("no usable sandbox")
        || t.contains("namespace sandbox")
        || t.contains("setuid sandbox")
    {
        Some(
            "the Chrome sandbox cannot start (user namespaces are blocked for this browser; on Ubuntu 23.10 and later an AppArmor profile has to allow them, which `sudo ./install-linux.sh --update --with-desktop` sets up for the browser it installs in /opt/praxi-browser)",
        )
    } else if t.contains("error while loading shared libraries") {
        Some("the browser is missing system libraries")
    } else {
        None
    }
}

/// One short summary of what the browser said: its `FATAL` and `ERROR` lines
/// (the cause), else the last lines.
#[must_use]
pub(crate) fn stderr_summary(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let causes: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.contains("FATAL") || l.contains("ERROR"))
        .take(2)
        .collect();
    let said = if causes.is_empty() {
        let n = lines.len().saturating_sub(3);
        lines[n..].join(" | ")
    } else {
        causes.join(" | ")
    };
    let said: String = said.chars().filter(|c| !c.is_control()).take(300).collect();
    match launch_hint(stderr) {
        Some(h) if said.is_empty() => h.to_owned(),
        Some(h) => format!("{h}. Browser said: {said}"),
        None if said.is_empty() => String::new(),
        None => format!("Browser said: {said}"),
    }
}

/// Parse a `DevToolsActivePort` file into a browser websocket URL.
pub fn parse_devtools_active_port(content: &str) -> Option<String> {
    let mut lines = content.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    path.starts_with('/')
        .then(|| format!("ws://127.0.0.1:{port}{path}"))
}

/// Wait for the browser to write `DevToolsActivePort`.
pub(crate) async fn wait_for_endpoint(
    profile: &Path,
    timeout: Duration,
    child: &mut tokio::process::Child,
) -> Result<String> {
    let file = profile.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(c) = tokio::fs::read_to_string(&file).await
            && let Some(url) = parse_devtools_active_port(&c)
        {
            return Ok(url);
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::Launch(format!("browser exited early ({status})")));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Launch(format!(
                "no DevToolsActivePort after {timeout:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Resolve `ws://...`, `http://host:port` or `host:port` into a websocket debugger URL.
pub async fn resolve_endpoint(endpoint: &str) -> Result<String> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_string());
    }
    let hostport = endpoint
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let body = http_get(&hostport, "/json/version").await?;
    let v: Value = serde_json::from_str(&body)?;
    v.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Launch("endpoint has no webSocketDebuggerUrl".into()))
}

/// Minimal HTTP/1.1 GET over a raw socket (avoids an HTTP client dependency).
async fn http_get(hostport: &str, path: &str) -> Result<String> {
    http_get_from(
        hostport,
        || async { TcpStream::connect(hostport).await },
        path,
    )
    .await
}

/// [`http_get`] to an address the caller already resolved and checked (nothing is resolved
/// again, so a name cannot answer differently the second time). The reply is capped: a
/// debugging endpoint that never stops talking cannot fill memory.
pub(crate) async fn http_get_at(
    addr: std::net::SocketAddr,
    host_header: &str,
    path: &str,
) -> Result<String> {
    http_get_from(
        host_header,
        || async move { TcpStream::connect(addr).await },
        path,
    )
    .await
}

/// Longest reply [`http_get_from`] reads, in bytes.
const HTTP_REPLY_MAX: u64 = 256 * 1024;

async fn http_get_from<F, Fut>(host_header: &str, connect: F, path: &str) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<TcpStream>>,
{
    let fut = async {
        let mut s = connect().await?;
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        s.take(HTTP_REPLY_MAX).read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
    };
    let raw = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .map_err(|_| Error::Timeout(format!("GET {host_header}{path}")))??;
    raw.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .ok_or_else(|| Error::Launch("malformed HTTP response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake file system for [`select_browser`]: what each path is.
    fn probe_of(table: &'static [(&'static str, Probe)]) -> impl Fn(&Path) -> Probe {
        move |p| {
            table
                .iter()
                .find(|(n, _)| Path::new(n) == p)
                .map_or(Probe::Missing, |(_, k)| *k)
        }
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_first_usable_candidate_wins_and_snap_ones_are_skipped() {
        let probe = probe_of(&[
            ("/snap/bin/chromium", Probe::Snap),
            ("/usr/bin/chromium", Probe::Snap),
            ("/opt/g/chrome", Probe::Usable),
            ("/data/browser/i-1/chrome-linux64/chrome", Probe::Usable),
        ]);
        let candidates = paths(&[
            "/snap/bin/chromium",
            "/usr/bin/chromium",
            "/opt/g/chrome",
            "/data/browser/i-1/chrome-linux64/chrome",
        ]);
        assert_eq!(
            select_browser(None, &candidates, &probe).unwrap(),
            Path::new("/opt/g/chrome")
        );
    }

    #[test]
    fn a_downloaded_browser_is_the_last_resort() {
        // The managed one comes last in `candidate_paths`: a system browser that works wins.
        let probe = probe_of(&[
            ("/usr/bin/google-chrome", Probe::Usable),
            ("/data/browser/i-1/chrome-linux64/chrome", Probe::Usable),
        ]);
        let candidates = paths(&[
            "/usr/bin/google-chrome",
            "/data/browser/i-1/chrome-linux64/chrome",
        ]);
        assert_eq!(
            select_browser(None, &candidates, &probe).unwrap(),
            Path::new("/usr/bin/google-chrome")
        );
        // With only the managed one present it is used.
        let only = probe_of(&[("/data/browser/i-1/chrome-linux64/chrome", Probe::Usable)]);
        assert!(select_browser(None, &candidates, &only).is_ok());
    }

    #[test]
    fn only_snap_browsers_give_an_error_that_names_the_snap_and_the_fix() {
        let probe = probe_of(&[("/snap/bin/chromium", Probe::Snap)]);
        let err = select_browser(
            None,
            &paths(&["/usr/bin/chromium", "/snap/bin/chromium"]),
            &probe,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("snap package") && err.contains("/snap/bin/chromium"),
            "{err}"
        );
        assert!(err.contains("BOT_BROWSER_PATH"), "{err}");
        #[cfg(target_os = "linux")]
        assert!(err.contains("bot browser install"), "{err}");
    }

    #[test]
    fn nothing_found_says_what_to_install() {
        let err = select_browser(None, &paths(&["/usr/bin/chromium"]), &probe_of(&[])).unwrap_err();
        assert!(matches!(err, Error::BrowserNotFound));
        let text = err.to_string();
        assert!(
            text.contains("no Chrome, Chromium or Edge was found"),
            "{text}"
        );
        assert!(text.contains("BOT_BROWSER_PATH"), "{text}");
        #[cfg(target_os = "linux")]
        assert!(text.contains("bot browser install"), "{text}");
    }

    #[test]
    fn an_override_wins_and_a_bad_one_is_an_error_not_a_fallback() {
        let probe = probe_of(&[
            ("/mine/chrome", Probe::Usable),
            ("/mine/snap", Probe::Snap),
            ("/usr/bin/chromium", Probe::Usable),
        ]);
        let candidates = paths(&["/usr/bin/chromium"]);
        assert_eq!(
            select_browser(Some(Path::new("/mine/chrome")), &candidates, &probe).unwrap(),
            Path::new("/mine/chrome")
        );
        let snap = select_browser(Some(Path::new("/mine/snap")), &candidates, &probe)
            .unwrap_err()
            .to_string();
        assert!(
            snap.contains("snap browser") && snap.contains("/mine/snap"),
            "{snap}"
        );
        let missing = select_browser(Some(Path::new("/mine/nope")), &candidates, &probe)
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains("/mine/nope") && missing.contains("does not exist"),
            "{missing}"
        );
    }

    #[cfg(unix)]
    fn mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn candidates_cover_fixed_places_once_and_skip_relative_path_folders() {
        let path = std::ffi::OsString::from("relative/bin:/usr/bin:/nonexistent-praxi-dir");
        let c = linux_candidates(Some(&path));
        assert!(!c.iter().any(|p| p.starts_with("relative")), "{c:?}");
        // A folder that does not exist (so is not one of ours) adds nothing.
        assert!(
            !c.iter().any(|p| p.starts_with("/nonexistent-praxi-dir")),
            "{c:?}"
        );
        let usr_bin = c
            .iter()
            .filter(|p| **p == Path::new("/usr/bin/chromium"))
            .count();
        assert_eq!(usr_bin, 1, "{c:?}");
        // Distro launcher scripts come before the programs behind them.
        let script = c
            .iter()
            .position(|p| p == Path::new("/usr/bin/chromium"))
            .unwrap();
        let real = c
            .iter()
            .position(|p| p == Path::new("/usr/lib/chromium/chromium"))
            .unwrap();
        assert!(script < real);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_folder_anyone_can_write_to_is_never_searched() {
        let d = tempfile::tempdir().unwrap();
        let mine = d.path().join("mine");
        let group = d.path().join("group");
        let open = d.path().join("open");
        for (dir, m) in [(&mine, 0o755), (&group, 0o775), (&open, 0o777)] {
            std::fs::create_dir(dir).unwrap();
            mode(dir, m);
        }
        let path = std::env::join_paths([&mine, &group, &open]).unwrap();
        let c = linux_candidates(Some(&path));
        assert!(c.contains(&mine.join("chromium")), "{c:?}");
        assert!(!c.iter().any(|p| p.starts_with(&group)), "group: {c:?}");
        assert!(!c.iter().any(|p| p.starts_with(&open)), "open: {c:?}");
        // The fixed distro places stay, whatever PATH says.
        assert!(c.contains(&PathBuf::from("/usr/bin/chromium")));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_a_path_folder_that_leads_outside_it_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        mode(&bin, 0o755);
        std::fs::write(bin.join("chromium-real"), b"x").unwrap();
        std::os::unix::fs::symlink("chromium-real", bin.join("chromium")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", bin.join("google-chrome")).unwrap();
        std::fs::write(bin.join("chromium-browser"), b"x").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        let c = linux_candidates(Some(&path));
        assert!(c.contains(&bin.join("chromium")), "inside link kept: {c:?}");
        assert!(c.contains(&bin.join("chromium-browser")));
        assert!(
            !c.contains(&bin.join("google-chrome")),
            "outside link dropped: {c:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_owner_and_mode_rule() {
        // (uid, mode, service uid)
        assert!(owner_and_mode_trusted(0, 0o755, 1000));
        assert!(owner_and_mode_trusted(1000, 0o700, 1000));
        assert!(!owner_and_mode_trusted(1001, 0o755, 1000), "someone else");
        assert!(!owner_and_mode_trusted(0, 0o775, 1000), "group-writable");
        assert!(!owner_and_mode_trusted(1000, 0o757, 1000), "world-writable");
        assert!(!owner_and_mode_trusted(0, 0o1777, 1000), "even with sticky");
    }

    #[cfg(unix)]
    #[test]
    fn a_browser_everyone_can_overwrite_is_unsafe_and_a_normal_one_is_usable() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("b");
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o755);
        let chrome = dir.join("chrome");
        std::fs::write(&chrome, b"#!/bin/sh\nexit 0\n").unwrap();
        mode(&chrome, 0o755);
        assert_eq!(probe_path(&chrome), Probe::Usable);
        // The file itself.
        mode(&chrome, 0o757);
        assert_eq!(probe_path(&chrome), Probe::Unsafe);
        mode(&chrome, 0o755);
        // The folder it lives in.
        mode(&dir, 0o777);
        assert_eq!(probe_path(&chrome), Probe::Unsafe);
        mode(&dir, 0o755);
        // A link is judged by where it really leads, not by where it sits.
        let linkdir = d.path().join("l");
        std::fs::create_dir(&linkdir).unwrap();
        mode(&linkdir, 0o755);
        std::os::unix::fs::symlink(&chrome, linkdir.join("chrome")).unwrap();
        assert_eq!(probe_path(&linkdir.join("chrome")), Probe::Usable);
        mode(&dir, 0o777);
        assert_eq!(probe_path(&linkdir.join("chrome")), Probe::Unsafe);
        mode(&dir, 0o755);
    }

    #[test]
    fn an_unsafe_override_is_refused_and_an_unsafe_candidate_is_skipped() {
        let probe = probe_of(&[
            ("/mine/open", Probe::Unsafe),
            ("/usr/bin/chromium", Probe::Unsafe),
            ("/opt/g/chrome", Probe::Usable),
        ]);
        let err = select_browser(
            Some(Path::new("/mine/open")),
            &paths(&["/opt/g/chrome"]),
            &probe,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("/mine/open") && err.contains("BOT_BROWSER_PATH") && err.contains("o-w"),
            "{err}"
        );
        assert_eq!(
            select_browser(
                None,
                &paths(&["/usr/bin/chromium", "/opt/g/chrome"]),
                &probe
            )
            .unwrap(),
            Path::new("/opt/g/chrome")
        );
        // Only an unsafe one around: nothing usable, and no pretence otherwise.
        assert!(matches!(
            select_browser(None, &paths(&["/usr/bin/chromium"]), &probe),
            Err(Error::BrowserNotFound)
        ));
    }

    /// An install folder shaped like a real one: marker, `chrome-linux64/chrome`.
    fn fake_install(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name).join("chrome-linux64");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chrome"), b"x").unwrap();
        std::fs::write(root.join(name).join(MANAGED_MARKER), b"m").unwrap();
        dir.join("chrome")
    }

    fn private_root() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        mode(d.path(), 0o700);
        d
    }

    #[test]
    fn a_finished_install_is_found_and_a_half_written_one_is_not() {
        let d = private_root();
        let yes = |_: &Path| true;
        assert_eq!(managed_browser(d.path(), &yes), None);
        // A folder without the marker (the install stopped before it was written).
        let half = d.path().join("i-1/chrome-linux64");
        std::fs::create_dir_all(&half).unwrap();
        std::fs::write(half.join("chrome"), b"x").unwrap();
        assert_eq!(managed_browser(d.path(), &yes), None);
        // The marker makes it count (once the manifest check agrees).
        std::fs::write(d.path().join("i-1").join(MANAGED_MARKER), b"m").unwrap();
        assert_eq!(managed_browser(d.path(), &yes), Some(half.join("chrome")));
        // A marker without the program does not.
        std::fs::create_dir_all(d.path().join("i-2")).unwrap();
        std::fs::write(d.path().join("i-2").join(MANAGED_MARKER), b"m").unwrap();
        assert_eq!(managed_browser(d.path(), &yes), Some(half.join("chrome")));
        // The downloads folder is never a browser.
        std::fs::create_dir_all(d.path().join("downloads")).unwrap();
        assert_eq!(managed_browser(d.path(), &yes), Some(half.join("chrome")));
    }

    #[test]
    fn a_marker_and_a_program_are_not_enough_without_a_passing_manifest() {
        let d = private_root();
        let forged = fake_install(d.path(), "i-forged");
        assert_eq!(managed_browser(d.path(), &|_: &Path| false), None);
        // The check is asked about the program, and its answer decides.
        let asked = std::cell::RefCell::new(Vec::new());
        let only_forged_fails = |b: &Path| {
            asked.borrow_mut().push(b.to_owned());
            false
        };
        assert_eq!(managed_browser(d.path(), &only_forged_fails), None);
        assert_eq!(*asked.borrow(), vec![forged.clone()]);
        assert_eq!(
            managed_browser(d.path(), &|b: &Path| b == forged),
            Some(forged)
        );
    }

    #[test]
    fn the_newest_install_that_passes_wins_and_a_failing_newer_one_is_passed_over() {
        let d = private_root();
        let old = fake_install(d.path(), "i-old");
        std::thread::sleep(Duration::from_millis(30));
        let new = fake_install(d.path(), "i-new");
        assert_eq!(
            managed_browser(d.path(), &|_: &Path| true),
            Some(new.clone())
        );
        assert_eq!(managed_browser(d.path(), &|b: &Path| b != new), Some(old));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_for_an_install_folder_or_for_the_program_is_never_followed() {
        let d = private_root();
        // An install folder that is a link to somewhere else.
        let elsewhere = tempfile::tempdir().unwrap();
        fake_install(elsewhere.path(), "i-real");
        std::os::unix::fs::symlink(elsewhere.path().join("i-real"), d.path().join("i-link"))
            .unwrap();
        assert_eq!(managed_browser(d.path(), &|_: &Path| true), None);
        // A program that is a link.
        let dir = d.path().join("i-2/chrome-linux64");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(d.path().join("i-2").join(MANAGED_MARKER), b"m").unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.join("chrome")).unwrap();
        assert_eq!(managed_browser(d.path(), &|_: &Path| true), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_data_folder_others_can_write_to_holds_no_usable_browser() {
        let d = private_root();
        fake_install(d.path(), "i-1");
        assert!(managed_browser(d.path(), &|_: &Path| true).is_some());
        mode(d.path(), 0o777);
        assert_eq!(managed_browser(d.path(), &|_: &Path| true), None);
        mode(d.path(), 0o775);
        assert_eq!(managed_browser(d.path(), &|_: &Path| true), None);
    }

    #[test]
    fn the_host_language_reaches_the_browser_before_the_page_and_never_over_a_choice() {
        let a = language_args(Some("tr-TR,tr".into()), &[]);
        assert_eq!(a, ["--lang=tr-TR", "--accept-lang=tr-TR,tr"]);
        let none: Vec<String> = Vec::new();
        assert_eq!(language_args(None, &[]), none);
        assert_eq!(
            language_args(Some("tr-TR,tr".into()), &["--lang=de-DE".into()]),
            none
        );
        let mut args = vec!["--x".to_owned(), "about:blank".to_owned()];
        insert_before_url(&mut args, a.clone());
        assert_eq!(args.last().unwrap(), "about:blank");
        assert_eq!(args[1], "--lang=tr-TR");
        let mut empty = vec!["about:blank".to_owned()];
        insert_before_url(&mut empty, vec!["--y".into()]);
        assert_eq!(empty, ["--y", "about:blank"]);
    }

    #[test]
    fn only_a_plain_language_list_gets_onto_the_command_line() {
        for ok in [
            "tr",
            "tr-TR",
            "tr-TR,tr",
            "en-US,en",
            "fil,fil-PH",
            "de-DE,de,en-US,en",
        ] {
            assert_eq!(language_args(Some(ok.into()), &[]).len(), 2, "{ok}");
        }
        for bad in [
            "",
            ",",
            "tr,",
            "TR",
            "t",
            "toolong",
            "tr_TR",
            "tr-tr",
            "tr-TUR",
            "tr TR",
            "en-US --no-sandbox",
            "en-US,--no-sandbox",
            "en\n--disable-web-security",
            "tr-TR;x",
            "tr-TR,tr,en,de,fr",
            "../../etc",
            "ü",
        ] {
            assert!(language_args(Some(bad.into()), &[]).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn dangerous_extra_flags_are_refused_whatever_the_spelling() {
        for bad in [
            "--no-sandbox",
            "--NO-SANDBOX",
            "-no-sandbox",
            "--no-sandbox=1",
            " --no-sandbox",
            "--disable-setuid-sandbox",
            "--disable-gpu-sandbox",
            "--disable-web-security",
            "--disable-site-isolation-trials",
            "--ignore-certificate-errors",
            "--ignore-certificate-errors-spki-list=abc",
            "--remote-debugging-port=9222",
            "--remote-debugging-address=0.0.0.0",
            "--remote-debugging-pipe",
            "--remote-allow-origins=*",
            "--user-data-dir=/tmp/x",
            "--load-extension=/tmp/ext",
            "--renderer-cmd-prefix=sh",
            "--gpu-launcher=sh",
            "--browser-subprocess-path=/tmp/x",
            "file:///etc/passwd",
            "about:blank",
            "",
        ] {
            assert!(extra_arg_denied(bad), "{bad:?}");
            assert!(
                matches!(check_extra_args(&[bad.to_owned()]), Err(Error::Launch(_))),
                "{bad:?}"
            );
        }
        for fine in [
            "--foo",
            "--lang=de-DE",
            "--window-position=0,0",
            "--mute-audio",
        ] {
            assert!(!extra_arg_denied(fine), "{fine}");
        }
        assert!(check_extra_args(&["--mute-audio".into(), "--foo=1".into()]).is_ok());
        let err = check_extra_args(&["--user-data-dir=/secret/dir".into()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--user-data-dir") && !err.contains("/secret/dir"),
            "{err}"
        );
    }

    #[test]
    fn denied_flags_never_reach_the_command_line_not_even_through_the_pure_builder() {
        let opts = LaunchOptions {
            args: [
                "--no-sandbox",
                "--disable-web-security",
                "--ignore-certificate-errors",
                "--remote-debugging-port=1",
                "--remote-debugging-address=0.0.0.0",
                "--user-data-dir=/evil",
                "--mute-audio",
            ]
            .map(String::from)
            .to_vec(),
            ..Default::default()
        };
        for linux in [false, true] {
            let a = build_args_for_host(&opts, Path::new("/p"), linux, true, false, None);
            assert!(a.contains(&"--mute-audio".to_owned()), "{a:?}");
            assert_eq!(
                a.iter()
                    .filter(|x| x.starts_with("--remote-debugging"))
                    .count(),
                1,
                "only our own port=0: {a:?}"
            );
            assert_eq!(
                a.iter()
                    .filter(|x| x.starts_with("--user-data-dir"))
                    .count(),
                1,
                "{a:?}"
            );
            for banned in [
                "--no-sandbox",
                "--disable-web-security",
                "--ignore-certificate",
            ] {
                assert!(!a.iter().any(|x| x.starts_with(banned)), "{banned}: {a:?}");
            }
        }
        // The one way the sandbox goes off: this module's own root decision.
        let a = build_args_for_host(
            &LaunchOptions::default(),
            Path::new("/p"),
            true,
            true,
            true,
            None,
        );
        assert!(a.contains(&"--no-sandbox".to_owned()));
    }

    #[test]
    fn headless_defaults_are_a_real_window_and_nothing_that_breaks_sites() {
        let a = build_args_for_host(
            &LaunchOptions::default(),
            Path::new("/p"),
            true,
            false,
            false,
            None,
        );
        assert!(a.contains(&"--headless=new".to_owned()));
        assert!(a.contains(&"--window-size=1280,800".to_owned()), "{a:?}");
        // Layout and rendering stay what a visitor's browser would do.
        assert!(!a.iter().any(|x| x == "--hide-scrollbars"), "{a:?}");
        // No disguise either: user agent, automation switches and webdriver stay as they are.
        for banned in [
            "--user-agent",
            "--disable-blink-features",
            "--enable-automation",
            "--disable-web-security",
            "--ignore-certificate-errors",
            "--no-sandbox",
        ] {
            assert!(!a.iter().any(|x| x.starts_with(banned)), "{banned}: {a:?}");
        }
    }

    #[test]
    fn recognizes_snap_wrappers_without_launching_them() {
        let path =
            std::env::temp_dir().join(format!("praxi-snap-wrapper-test-{}", std::process::id()));
        std::fs::write(&path, b"#!/bin/sh\nexec /snap/bin/chromium \"$@\"\n").unwrap();
        assert!(is_snap_browser(&path));
        std::fs::write(
            &path,
            b"#!/bin/sh\nexec /usr/lib/chromium/chromium \"$@\"\n",
        )
        .unwrap();
        assert!(!is_snap_browser(&path));
        std::fs::remove_file(path).unwrap();
    }

    /// Review blocker 2: a served session keeps the popup blocker, sends
    /// everything through its proxy, and extra flags cannot loosen that.
    #[test]
    fn a_served_session_is_proxied_and_keeps_the_popup_blocker() {
        let own = build_args_for_host(
            &LaunchOptions::default(),
            Path::new("/p"),
            false,
            true,
            false,
            None,
        );
        assert!(own.contains(&"--disable-popup-blocking".to_owned()));
        let served = LaunchOptions {
            served_proxy: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 4567))),
            args: vec![
                "--proxy-server=direct://".into(),
                "--disable-popup-blocking".into(),
                "--proxy-bypass-list=*".into(),
            ],
            ..LaunchOptions::default()
        };
        let a = build_args_for_host(&served, Path::new("/p"), false, true, false, None);
        assert!(!a.contains(&"--disable-popup-blocking".to_owned()), "{a:?}");
        assert!(a.contains(&"--proxy-server=http://127.0.0.1:4567".to_owned()));
        assert_eq!(
            a.iter().filter(|x| x.starts_with("--proxy-server")).count(),
            1,
            "{a:?}"
        );
        assert!(a.contains(&"--proxy-bypass-list=<-loopback>".to_owned()));
        assert!(!a.contains(&"--proxy-bypass-list=*".to_owned()));
    }

    #[test]
    fn launch_errors_name_the_cause_of_a_dead_browser() {
        let x = "[1:1:ERROR] Missing X server or $DISPLAY
[1:1] The platform failed to initialize.  Exiting.
";
        let msg = stderr_summary(x);
        assert!(msg.contains("X display is not reachable"), "{msg}");
        assert!(msg.contains("Browser said:"), "{msg}");
        assert!(launch_hint("No usable sandbox! Update your kernel").is_some());
        assert_eq!(stderr_summary(""), "");
        assert_eq!(launch_hint("harmless noise"), None);
    }

    /// The Ubuntu 24.04 case seen on a real host: the cause is the first line and a
    /// long crash dump follows it.
    #[test]
    fn the_cause_survives_a_crash_dump_after_it() {
        let first = "[1:1:1127/074217:FATAL:zygote_host_impl_linux.cc:144] No usable sandbox! If you are running on Ubuntu 23.10+\n";
        let mut keep = StderrKeep::default();
        keep.push(first.as_bytes());
        for i in 0..200 {
            keep.push(format!("#{i} 0x00007f0000{i:04x} frame\n").as_bytes());
        }
        keep.push(b"[end of stack trace]\n");
        let text = keep.text();
        assert!(text.contains("No usable sandbox"), "{text}");
        assert!(text.contains("[end of stack trace]"), "{text}");
        assert!(
            text.len() < STDERR_HEAD + STDERR_KEEP + 32,
            "{}",
            text.len()
        );
        let msg = stderr_summary(&text);
        assert!(msg.contains("Chrome sandbox cannot start"), "{msg}");
        assert!(msg.contains("AppArmor"), "{msg}");
        assert!(msg.contains("No usable sandbox"), "{msg}");
        assert!(!msg.contains("frame"), "{msg}");
    }

    #[test]
    fn a_short_stderr_is_kept_whole_and_without_a_gap() {
        let mut keep = StderrKeep::default();
        keep.push(b"one\n");
        keep.push(b"two\n");
        assert_eq!(keep.text(), "one\ntwo\n");
    }

    #[test]
    fn parses_active_port_file() {
        let url = parse_devtools_active_port("9222\n/devtools/browser/abc-123\n").unwrap();
        assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/abc-123");
        assert!(parse_devtools_active_port("9222\n").is_none());
        assert!(parse_devtools_active_port("x\n/y").is_none());
    }

    #[test]
    fn persistent_profiles_keep_native_cookie_protection() {
        let opts = LaunchOptions {
            user_data_dir: Some("/managed/profile".into()),
            ..Default::default()
        };
        let args = build_args_for_host(
            &opts,
            Path::new("/managed/profile"),
            false,
            true,
            false,
            None,
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "--password-store=basic" || a == "--use-mock-keychain")
        );
    }

    #[test]
    fn args_are_isolated_and_headless() {
        let o = LaunchOptions {
            args: vec!["--foo".into()],
            ..Default::default()
        };
        let a = build_args_for_host(&o, Path::new("/tmp/p"), false, true, false, None);
        assert!(a.contains(&"--remote-debugging-port=0".to_string()));
        assert!(a.iter().any(|x| x.starts_with("--user-data-dir=")));
        assert!(a.contains(&"--headless=new".to_string()));
        assert!(a.contains(&"--foo".to_string()));
        assert_eq!(a.last().unwrap(), "about:blank");
        let headed = build_args_for_host(
            &LaunchOptions {
                headless: false,
                ..Default::default()
            },
            Path::new("/tmp/p"),
            true,
            true,
            false,
            None,
        );
        assert!(!headed.contains(&"--headless=new".to_string()));
    }

    /// Owner round 5: the bot's Chrome showed "Restore pages?" and the
    /// "only for automated testing" bar, and sat small on the virtual screen.
    #[test]
    fn every_launch_hides_the_restore_bubble_and_the_testing_bar() {
        for (linux, display) in [(false, true), (true, true), (true, false)] {
            let a = build_args_for_host(
                &LaunchOptions::default(),
                Path::new("/p"),
                linux,
                display,
                false,
                None,
            );
            for flag in [
                "--hide-crash-restore-bubble",
                "--disable-session-crashed-bubble",
                "--noerrdialogs",
                "--disable-infobars",
                "--no-first-run",
                "--no-default-browser-check",
            ] {
                assert!(a.iter().any(|x| x == flag), "{flag} missing: {a:?}");
            }
            // None of this loosens the browser's own safety.
            assert!(!a.iter().any(|x| x.contains("site-per-process")));
            assert!(!a.iter().any(|x| x.starts_with("--disable-web-security")));
            assert!(!a.iter().any(|x| x == "--no-sandbox"));
        }
    }

    #[test]
    fn a_headed_linux_browser_fills_the_virtual_screen() {
        let headed = LaunchOptions {
            headless: false,
            ..LaunchOptions::default()
        };
        let a = build_args_for_host(
            &headed,
            Path::new("/p"),
            true,
            true,
            false,
            Some((1920, 1080)),
        );
        assert!(a.contains(&"--start-maximized".to_owned()), "{a:?}");
        assert!(a.contains(&"--window-position=0,0".to_owned()), "{a:?}");
        assert!(a.contains(&"--window-size=1920,1080".to_owned()), "{a:?}");
        assert!(!a.contains(&"--window-size=1280,800".to_owned()), "{a:?}");
        // The screen could not be read: still maximised, at the requested size.
        let a = build_args_for_host(&headed, Path::new("/p"), true, true, false, None);
        assert!(a.contains(&"--start-maximized".to_owned()));
        assert!(a.contains(&"--window-size=1280,800".to_owned()));
        // Headless (no window) and a person's own PC are left alone.
        let headless = build_args_for_host(
            &LaunchOptions::default(),
            Path::new("/p"),
            true,
            true,
            false,
            Some((1920, 1080)),
        );
        assert!(!headless.contains(&"--start-maximized".to_owned()));
        assert!(headless.contains(&"--window-size=1280,800".to_owned()));
        let own_pc = build_args_for_host(
            &headed,
            Path::new("/p"),
            false,
            true,
            false,
            Some((1920, 1080)),
        );
        assert!(!own_pc.contains(&"--start-maximized".to_owned()));
        assert!(own_pc.contains(&"--window-size=1280,800".to_owned()));
        // No display on Linux means headless, so nothing to maximise.
        let no_display = build_args_for_host(&headed, Path::new("/p"), true, false, false, None);
        assert!(!no_display.contains(&"--start-maximized".to_owned()));
    }

    #[test]
    fn linux_flags_follow_display_and_effective_uid() {
        let opts = LaunchOptions {
            headless: false,
            args: vec!["--no-sandbox".into(), "--headless=false".into()],
            ..Default::default()
        };
        for display in [false, true] {
            for root in [false, true] {
                let args =
                    build_args_for_host(&opts, Path::new("/profile"), true, display, root, None);
                assert_eq!(args.iter().any(|s| s == "--headless=new"), !display);
                assert_eq!(args.iter().any(|s| s == "--no-sandbox"), root);
                assert!(!args.iter().any(|s| s == "--headless=false"));
                assert!(args.iter().any(|s| s == "--disable-gpu"));
                assert!(args.iter().any(|s| s == "--disable-dev-shm-usage"));
            }
        }
        let opts = LaunchOptions::default();
        assert!(
            build_args_for_host(&opts, Path::new("/p"), true, true, false, None)
                .iter()
                .any(|s| s == "--headless=new")
        );
    }

    #[tokio::test]
    async fn resolves_http_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 512];
            let _ = s.read(&mut b).await.unwrap();
            let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        let url = resolve_endpoint(&format!("http://{addr}")).await.unwrap();
        assert_eq!(url, "ws://127.0.0.1:1/devtools/browser/x");
        assert_eq!(resolve_endpoint("ws://h:1/x").await.unwrap(), "ws://h:1/x");
    }
}
