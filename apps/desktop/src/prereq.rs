//! First-run prerequisite check (Windows-first, harmless elsewhere).
//!
//! The app silently depends on a few things: the `WebView2` runtime, the bundled `bot` sidecar,
//! a writable data folder on a local disk, a sane clock and a free daemon port. When the local
//! service does not come up, the start-up screen asks [`check`] what is wrong and shows one
//! plain sentence with one fix button instead of a generic "service not responding".
//!
//! [`evaluate`] is pure (it works on a [`Snapshot`]) so every failure can be injected in tests;
//! [`gather`] reads the real machine. Everything here is read-only and offline: the only write is
//! a tiny probe file that is removed again, the only network use is none. The one network action,
//! opening the official Microsoft `WebView2` page, is a separate explicit command.

#[cfg(test)]
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// The official Microsoft page for the `WebView2` runtime. Fixed: no command takes a URL.
pub const WEBVIEW2_DOWNLOAD_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Oldest `WebView2` major version the app is built and tested against.
const MIN_WEBVIEW2_MAJOR: u32 = 100;
/// 2025-01-01T00:00:00Z: a clock earlier than this has lost its battery or never been set.
const CLOCK_MIN_UNIX: u64 = 1_735_689_600;
/// 2100-01-01T00:00:00Z.
const CLOCK_MAX_UNIX: u64 = 4_102_444_800;

/// What the one primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fix {
    /// Check again after the person fixed it.
    Retry,
    /// Show the data folder in the file manager.
    OpenDataFolder,
    /// Open the official `WebView2` download page.
    DownloadWebview2,
}

/// One thing that is wrong, in a form the UI words (`errors.prereq.<id>.*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Stable id, also the i18n key.
    pub id: &'static str,
    /// The button that helps most.
    pub fix: Fix,
    /// `true` when the app cannot work until it is fixed; `false` is advice only.
    pub blocking: bool,
}

/// What the data folder looked like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirState {
    /// A directory we could write a probe file into (or create).
    Writable,
    /// Not a directory, or the probe write failed.
    Unusable(String),
}

/// Everything [`evaluate`] looks at; filled by [`gather`] or by a test.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Installed `WebView2` runtime version; `None` when there is none.
    pub webview2: Option<String>,
    /// Whether the `bot` sidecar file exists.
    pub sidecar_exists: bool,
    /// State of the data folder.
    pub data_dir: DirState,
    /// Whether the data folder is on a network share.
    pub data_dir_on_network: bool,
    /// Seconds since 1970 according to the clock.
    pub now_unix: u64,
    /// Whether another program holds the daemon port (only asked when nothing of ours runs).
    pub port_busy: bool,
}

/// `WebView2` version string (`120.0.2210.91`) to its major number.
fn webview2_major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// A path on a network share: `\\server\share\...` (not the `\\?\C:\` long-path prefix).
#[must_use]
pub fn is_network_path(path: &str) -> bool {
    let p = path.replace('/', "\\");
    p.starts_with("\\\\") && !p.starts_with("\\\\?\\") && !p.starts_with("\\\\.\\")
}

/// Blocking problems first, in the order a person should fix them.
#[must_use]
pub fn evaluate(s: &Snapshot) -> Vec<Issue> {
    let mut out = Vec::new();
    match &s.webview2 {
        None => out.push(Issue {
            id: "webview2_missing",
            fix: Fix::DownloadWebview2,
            blocking: true,
        }),
        Some(v) if webview2_major(v).is_none_or(|m| m < MIN_WEBVIEW2_MAJOR) => out.push(Issue {
            id: "webview2_old",
            fix: Fix::DownloadWebview2,
            blocking: false,
        }),
        Some(_) => {}
    }
    if !s.sidecar_exists {
        out.push(Issue {
            id: "sidecar_missing",
            fix: Fix::Retry,
            blocking: true,
        });
    }
    if matches!(s.data_dir, DirState::Unusable(_)) {
        out.push(Issue {
            id: "data_dir_unusable",
            fix: Fix::OpenDataFolder,
            blocking: true,
        });
    }
    if s.data_dir_on_network {
        out.push(Issue {
            id: "data_dir_network",
            fix: Fix::OpenDataFolder,
            blocking: false,
        });
    }
    if s.port_busy {
        out.push(Issue {
            id: "port_busy",
            fix: Fix::Retry,
            blocking: true,
        });
    }
    if !(CLOCK_MIN_UNIX..CLOCK_MAX_UNIX).contains(&s.now_unix) {
        out.push(Issue {
            id: "clock_wrong",
            fix: Fix::Retry,
            blocking: false,
        });
    }
    out.sort_by_key(|i| !i.blocking); // stable: keeps the order above inside each group
    out
}

/// The answer of the `prereq_check` command.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Problems found, blocking ones first; empty when every check passed.
    pub issues: Vec<Issue>,
    /// Technical details for the Advanced section, with the user's name and home folder removed.
    pub details: String,
}

/// Replace the home folder and the account name by `~` / `<user>` so the text can be shared.
#[must_use]
pub fn redact(text: &str, home: Option<&str>, user: Option<&str>) -> String {
    let mut out = text.to_owned();
    for h in home.into_iter().filter(|h| h.len() > 3) {
        // Windows paths are case-insensitive and may use either slash.
        for variant in [h.to_owned(), h.replace('\\', "/")] {
            out = replace_ci(&out, &variant, "~");
        }
    }
    for u in user.into_iter().filter(|u| u.len() > 2) {
        out = replace_ci(&out, u, "<user>");
    }
    out
}

fn replace_ci(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let (lt, ln) = (text.to_lowercase(), needle.to_lowercase());
    if lt.len() != text.len() {
        return text.replace(needle, with); // case folding changed the length: exact match only
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = 0;
    while let Some(i) = lt[rest..].find(&ln) {
        out.push_str(&text[rest..rest + i]);
        out.push_str(with);
        rest += i + ln.len();
    }
    out.push_str(&text[rest..]);
    out
}

/// The facts behind the issues, one per line (redacted by the caller).
#[must_use]
pub fn details(s: &Snapshot, data_dir: &str, sidecar: &str, port: u16) -> String {
    let dir = match &s.data_dir {
        DirState::Writable => "writable".to_owned(),
        DirState::Unusable(why) => format!("not usable ({why})"),
    };
    [
        format!(
            "webview2: {}",
            s.webview2.as_deref().unwrap_or("not installed")
        ),
        format!(
            "service file: {sidecar} (present: {})",
            if s.sidecar_exists { "yes" } else { "no" }
        ),
        format!(
            "data folder: {data_dir} ({dir}, {})",
            if s.data_dir_on_network {
                "network share"
            } else {
                "local"
            }
        ),
        format!(
            "service port {port}: {}",
            if s.port_busy { "busy" } else { "free" }
        ),
        format!("clock (unix seconds): {}", s.now_unix),
    ]
    .join("\n")
}

/// Try to create (and remove) a probe file in `dir`, creating `dir` when it is missing.
fn probe_dir(dir: &Path) -> DirState {
    if dir.exists() && !dir.is_dir() {
        return DirState::Unusable("it is a file, not a folder".to_owned());
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        return DirState::Unusable(e.to_string());
    }
    let probe = dir.join(format!(".prereq-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            DirState::Writable
        }
        Err(e) => DirState::Unusable(e.to_string()),
    }
}

/// Read the real machine. `daemon_running` skips the port probe while our own service holds it.
#[must_use]
pub fn gather(data_dir: &Path, sidecar: &Path, port: u16, daemon_running: bool) -> Snapshot {
    Snapshot {
        webview2: tauri::webview_version()
            .ok()
            .filter(|v| !v.trim().is_empty()),
        sidecar_exists: sidecar.is_file(),
        data_dir: probe_dir(data_dir),
        data_dir_on_network: is_network_path(&data_dir.to_string_lossy()),
        now_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        // A busy preferred port is recoverable: startup reserves a free loopback port.
        port_busy: !daemon_running && process_guard::lifecycle::bind(port).is_err(),
    }
}

/// Run every check and build the report (redacted details included).
#[must_use]
pub fn check(data_dir: &Path, sidecar: &Path, port: u16, daemon_running: bool) -> Report {
    let snap = gather(data_dir, sidecar, port, daemon_running);
    let issues = evaluate(&snap);
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok();
    let details = redact(
        &details(
            &snap,
            &data_dir.to_string_lossy(),
            &sidecar.to_string_lossy(),
            port,
        ),
        home.as_deref(),
        user.as_deref(),
    );
    Report { issues, details }
}

/// Words of the native dialog shown when `WebView2` is missing and no window can open at all.
/// `lang` is a locale such as `tr-TR`; unknown languages get English.
#[must_use]
pub fn missing_webview2_text(lang: &str) -> (&'static str, &'static str) {
    match lang.get(..2).unwrap_or("").to_ascii_lowercase().as_str() {
        "tr" => (
            "Praxi-Bot açılamıyor",
            "Praxi-Bot için Microsoft WebView2 gerekli, ama bilgisayarında kurulu değil.\n\nMicrosoft'un resmî indirme sayfası açılsın mı? Kurduktan sonra Praxi-Bot'u yeniden aç.",
        ),
        "de" => (
            "Praxi-Bot kann nicht starten",
            "Praxi-Bot braucht Microsoft WebView2, das auf diesem Computer nicht installiert ist.\n\nSoll die offizielle Download-Seite von Microsoft geöffnet werden? Starte Praxi-Bot nach der Installation erneut.",
        ),
        _ => (
            "Praxi-Bot cannot start",
            "Praxi-Bot needs Microsoft WebView2, which is not installed on this computer.\n\nOpen Microsoft's official download page? After installing it, start Praxi-Bot again.",
        ),
    }
}

/// Before any window exists: when `WebView2` is missing, say so in plain words (the web UI cannot
/// show anything without it) and offer the official download page. Returns `true` when the app
/// should stop. Windows only; elsewhere it never stops the app.
#[cfg(windows)]
#[must_use]
#[allow(unsafe_code)] // MessageBoxW has no safe wrapper; the strings are NUL-terminated locals
pub fn stop_if_webview2_missing() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONERROR, MB_YESNO, MessageBoxW};
    use windows::core::PCWSTR;

    if tauri::webview_version().is_ok_and(|v| !v.trim().is_empty()) {
        return false;
    }
    let (title, body) = missing_webview2_text(&ui_language());
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (t, b) = (wide(title), wide(body));
    // SAFETY: both buffers are valid, NUL-terminated and outlive the call.
    let answer = unsafe {
        MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_YESNO | MB_ICONERROR,
        )
    };
    if answer == IDYES {
        let _ = open::that(WEBVIEW2_DOWNLOAD_URL);
    }
    true
}

/// Windows UI language as `xx-YY` (empty when unknown).
#[cfg(windows)]
#[allow(unsafe_code)] // GetUserDefaultLocaleName writes into our buffer
fn ui_language() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    // SAFETY: `buf` is a valid writable buffer of the documented maximum locale-name size.
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    usize::try_from(n)
        .ok()
        .filter(|n| *n > 1)
        .map(|n| String::from_utf16_lossy(&buf[..n - 1]))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000; // 2027

    fn healthy() -> Snapshot {
        Snapshot {
            webview2: Some("131.0.2903.70".to_owned()),
            sidecar_exists: true,
            data_dir: DirState::Writable,
            data_dir_on_network: false,
            now_unix: NOW,
            port_busy: false,
        }
    }

    fn ids(s: &Snapshot) -> Vec<&'static str> {
        evaluate(s).iter().map(|i| i.id).collect()
    }

    #[test]
    fn a_healthy_machine_has_no_issue() {
        assert_eq!(ids(&healthy()), Vec::<&str>::new());
    }

    #[test]
    fn missing_webview2_blocks_and_points_to_the_download() {
        let mut s = healthy();
        s.webview2 = None;
        let issues = evaluate(&s);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "webview2_missing");
        assert_eq!(issues[0].fix, Fix::DownloadWebview2);
        assert!(issues[0].blocking);
    }

    #[test]
    fn old_or_unreadable_webview2_is_advice_only() {
        for v in ["87.0.664.60", "garbage"] {
            let mut s = healthy();
            s.webview2 = Some(v.to_owned());
            let issues = evaluate(&s);
            assert_eq!(issues[0].id, "webview2_old", "{v}");
            assert!(!issues[0].blocking);
        }
    }

    #[test]
    fn quarantined_sidecar_is_reported() {
        let mut s = healthy();
        s.sidecar_exists = false;
        assert_eq!(ids(&s), ["sidecar_missing"]);
    }

    #[test]
    fn unwritable_data_folder_offers_to_open_it() {
        let mut s = healthy();
        s.data_dir = DirState::Unusable("access denied".to_owned());
        let issues = evaluate(&s);
        assert_eq!(issues[0].id, "data_dir_unusable");
        assert_eq!(issues[0].fix, Fix::OpenDataFolder);
    }

    #[test]
    fn network_share_and_bad_clock_are_advice_after_blocking_problems() {
        let mut s = healthy();
        s.data_dir_on_network = true;
        s.now_unix = 946_684_800; // year 2000
        s.port_busy = true;
        assert_eq!(ids(&s), ["port_busy", "data_dir_network", "clock_wrong"]);
        s.now_unix = CLOCK_MAX_UNIX + 1;
        assert!(ids(&s).contains(&"clock_wrong"));
    }

    #[test]
    fn network_paths_are_told_from_local_ones() {
        assert!(is_network_path(r"\\nas\share\praxi"));
        assert!(is_network_path("//nas/share/praxi"));
        assert!(!is_network_path(r"\\?\C:\Users\me\praxi"));
        assert!(!is_network_path(r"C:\Users\me\praxi"));
        assert!(!is_network_path("/home/me/praxi"));
    }

    #[test]
    fn redact_removes_home_and_user_case_insensitively() {
        let text = r"data folder: C:\Users\Alice\AppData\Praxi (user alice)";
        let out = redact(text, Some(r"C:\Users\Alice"), Some("alice"));
        assert_eq!(out, r"data folder: ~\AppData\Praxi (user <user>)");
        assert!(!out.to_lowercase().contains("alice"));
        // Forward-slash spelling of the same home folder is removed too.
        let out = redact("C:/users/alice/x", Some(r"C:\Users\Alice"), None);
        assert_eq!(out, "~/x");
    }

    #[test]
    fn probe_dir_reports_a_file_in_the_way_and_cleans_up_after_itself() {
        let base = std::env::temp_dir().join(format!("praxi-prereq-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("not-a-folder");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(probe_dir(&file), DirState::Unusable(_)));
        let ok = base.join("new").join("data");
        assert_eq!(probe_dir(&ok), DirState::Writable);
        assert_eq!(
            std::fs::read_dir(&ok).unwrap().count(),
            0,
            "probe file removed"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_taken_preferred_port_does_not_block_startup_when_a_fallback_is_available() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let dir = std::env::temp_dir();
        let me = std::env::current_exe().unwrap();
        assert!(!gather(&dir, &me, port, false).port_busy);
        assert!(!gather(&dir, &me, port, true).port_busy);
    }

    #[test]
    fn the_native_dialog_speaks_en_tr_de() {
        let en = missing_webview2_text("en-US");
        let tr = missing_webview2_text("tr-TR");
        let de = missing_webview2_text("de-DE");
        assert_ne!(en, tr);
        assert_ne!(en, de);
        assert_ne!(tr, de);
        assert_eq!(missing_webview2_text("fr-FR"), en);
        assert_eq!(missing_webview2_text(""), en);
    }

    #[test]
    fn download_url_is_microsofts_official_https_page() {
        assert!(WEBVIEW2_DOWNLOAD_URL.starts_with("https://developer.microsoft.com/"));
    }
}
