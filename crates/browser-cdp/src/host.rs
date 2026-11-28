//! Small host probes. These never launch a browser.

/// Whether DISPLAY names an X11 display (including Xwayland/Xvfb).
#[must_use]
pub fn display_configured(display: Option<&str>) -> bool {
    display.is_some_and(|s| !s.trim().is_empty())
}

/// Session reachability. The X11 handshake runs on a dedicated OS thread, never
/// on a Tokio worker, bounded to 1 s. A reachable display is remembered for the
/// process life; an unreachable one is probed again after `RETRY_AFTER`, so a
/// slow handshake during a busy daemon start (or an Xvfb that came up late) does
/// not pin Chrome to headless until the next restart.
#[must_use]
pub fn has_display() -> bool {
    #[cfg(target_os = "linux")]
    {
        static DISPLAY: std::sync::Mutex<ProbeCache> = std::sync::Mutex::new(ProbeCache::new());
        let mut cache = DISPLAY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.get(std::time::Instant::now(), || {
            display_configured(std::env::var("DISPLAY").ok().as_deref())
                && bounded_probe(std::time::Duration::from_secs(1), || {
                    x11rb::connect(None).is_ok()
                })
        })
    }
    #[cfg(windows)]
    {
        interactive_station()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        true
    }
}

/// Size in pixels of the X screen the browser would open on (`None` off Linux
/// or when the display cannot be read within a second). The bot's browser
/// window takes this size on its virtual display.
#[must_use]
pub fn screen_size() -> Option<(u32, u32)> {
    #[cfg(target_os = "linux")]
    {
        bounded_value(std::time::Duration::from_secs(1), || {
            use x11rb::connection::Connection;
            let (conn, num) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots.get(num)?;
            Some((
                u32::from(root.width_in_pixels),
                u32::from(root.height_in_pixels),
            ))
        })
        .flatten()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// How long an unreachable result is trusted before the display is probed again.
#[cfg(any(target_os = "linux", test))]
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(15);

/// Positive results stick; negative ones expire.
#[cfg(any(target_os = "linux", test))]
struct ProbeCache {
    reachable: bool,
    failed_at: Option<std::time::Instant>,
}

#[cfg(any(target_os = "linux", test))]
impl ProbeCache {
    const fn new() -> Self {
        Self {
            reachable: false,
            failed_at: None,
        }
    }

    fn get(&mut self, now: std::time::Instant, probe: impl FnOnce() -> bool) -> bool {
        if self.reachable {
            return true;
        }
        if self
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < RETRY_AFTER)
        {
            return false;
        }
        if probe() {
            self.reachable = true;
        } else {
            self.failed_at = Some(now);
        }
        self.reachable
    }
}

#[cfg(any(target_os = "linux", test))]
fn bounded_probe(
    timeout: std::time::Duration,
    probe: impl FnOnce() -> bool + Send + 'static,
) -> bool {
    bounded_value(timeout, probe).unwrap_or(false)
}

/// Run `work` on its own OS thread and give up on it after `timeout`.
#[cfg(any(target_os = "linux", test))]
fn bounded_value<T: Send + 'static>(
    timeout: std::time::Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("display-probe".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.recv_timeout(timeout).ok()
}

#[cfg(windows)]
#[allow(unsafe_code)] // Query the current process station into a fixed-size Win32 structure.
fn interactive_station() -> bool {
    use windows::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
    };
    let mut flags = USEROBJECTFLAGS::default();
    unsafe {
        GetProcessWindowStation().is_ok_and(|station| {
            GetUserObjectInformationW(
                windows::Win32::Foundation::HANDLE(station.0),
                UOI_FLAGS,
                Some(std::ptr::from_mut(&mut flags).cast()),
                u32::try_from(std::mem::size_of_val(&flags)).unwrap_or(0),
                None,
            )
            .is_ok()
                && station_visible(flags.dwFlags)
        })
    }
}

#[cfg(any(windows, test))]
fn station_visible(flags: u32) -> bool {
    flags & 1 != 0 // WSF_VISIBLE; Session 0's noninteractive station has no flag.
}

/// Physical RAM in bytes. Only refresh memory, never enumerate processes.
#[must_use]
pub fn total_ram_bytes() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.total_memory()
}

/// Conservative default when memory cannot be determined.
#[must_use]
pub fn default_computer_runs(ram: u64) -> usize {
    if ram < 8 * 1024 * 1024 * 1024 { 1 } else { 2 }
}

/// Browser languages (`tr-TR,tr`) for a POSIX locale name such as
/// `tr_TR.UTF-8` or `sr_RS@latin`; `None` for `C`/`POSIX`/empty or anything
/// that is not `ll` or `ll_RR`. A browser started by a service has no
/// language of its own and would announce `en-US` whatever the host uses.
#[must_use]
pub fn locale_languages(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next()?.trim();
    let (lang, region) = match base.split_once('_') {
        Some((l, r)) => (l, Some(r)),
        None => (base, None),
    };
    let lang_ok = (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase());
    // `C` and `POSIX` are "no language chosen", not a language.
    if !lang_ok || matches!(lang, "c" | "posix") {
        return None;
    }
    match region {
        None => Some(lang.to_owned()),
        Some(r) if r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase()) => {
            Some(format!("{lang}-{r},{lang}"))
        }
        Some(_) => None,
    }
}

/// `LANG=...` from the text of `/etc/default/locale` or `/etc/locale.conf`.
fn lang_assignment(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("LANG=")?;
        Some(v.trim().trim_matches(['"', '\'']).to_owned())
    })
}

/// The host's language for the browser: the first set one of `LC_ALL`,
/// `LC_MESSAGES`, `LANG`, else the system-wide file (a systemd service has no
/// locale in its environment). `None` when the host has none, or uses `C`.
#[must_use]
pub fn host_languages() -> Option<String> {
    host_languages_from(|k| std::env::var(k).ok(), &|| {
        ["/etc/default/locale", "/etc/locale.conf"]
            .iter()
            .find_map(|f| std::fs::read_to_string(f).ok())
    })
}

/// [`host_languages`] with the environment and the system file injected.
pub fn host_languages_from(
    env: impl Fn(&str) -> Option<String>,
    system_file: &dyn Fn() -> Option<String>,
) -> Option<String> {
    let chosen = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| env(k).filter(|v| !v.trim().is_empty()))
        .or_else(|| system_file().and_then(|t| lang_assignment(&t)))?;
    locale_languages(&chosen)
}

/// `Area/City` style zone name, or `None` when it is not one (a path, `..`,
/// odd characters): only these reach the browser's environment.
fn iana_name(name: &str) -> Option<&str> {
    let n = name.trim();
    let ok = !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('/')
        && n.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'));
    ok.then_some(n)
}

/// The host's time zone to hand to the browser as `TZ`, when the process has
/// none of its own: the name in `/etc/timezone`, else the target of the
/// `/etc/localtime` link (`.../zoneinfo/Europe/Istanbul`). `None` when `TZ` is
/// already set (the browser inherits it) or the zone cannot be named.
#[must_use]
pub fn host_timezone() -> Option<String> {
    host_timezone_from(
        std::env::var("TZ").ok().as_deref(),
        std::fs::read_to_string("/etc/timezone").ok().as_deref(),
        std::fs::read_link("/etc/localtime").ok().as_deref(),
    )
}

/// [`host_timezone`] with its three inputs injected.
#[must_use]
pub fn host_timezone_from(
    tz_env: Option<&str>,
    etc_timezone: Option<&str>,
    localtime_target: Option<&std::path::Path>,
) -> Option<String> {
    if tz_env.is_some_and(|t| !t.trim().is_empty()) {
        return None;
    }
    if let Some(n) = etc_timezone
        .and_then(|t| t.lines().next())
        .and_then(iana_name)
    {
        return Some(n.to_owned());
    }
    let target = localtime_target?.to_str()?;
    let (_, zone) = target.rsplit_once("zoneinfo/")?;
    iana_name(zone).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locales_become_browser_languages() {
        assert_eq!(locale_languages("tr_TR.UTF-8").as_deref(), Some("tr-TR,tr"));
        assert_eq!(locale_languages("en_US").as_deref(), Some("en-US,en"));
        assert_eq!(locale_languages("sr_RS@latin").as_deref(), Some("sr-RS,sr"));
        assert_eq!(locale_languages("de").as_deref(), Some("de"));
        for none in [
            "", "C", "C.UTF-8", "POSIX", "en_us", "x", "tr_TR_X", "../etc", "tr-TR",
        ] {
            assert_eq!(locale_languages(none), None, "{none:?}");
        }
    }

    #[test]
    fn the_host_language_comes_from_the_environment_then_the_system_file() {
        let none = || None;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        // LC_ALL beats LANG; an empty one is ignored.
        let both = env(&[("LC_ALL", "de_DE.UTF-8"), ("LANG", "tr_TR.UTF-8")]);
        assert_eq!(
            host_languages_from(both, &none).as_deref(),
            Some("de-DE,de")
        );
        let empty = env(&[("LC_ALL", ""), ("LANG", "tr_TR.UTF-8")]);
        assert_eq!(
            host_languages_from(empty, &none).as_deref(),
            Some("tr-TR,tr")
        );
        // A service has no environment: the system-wide file speaks.
        let file = || Some("# comment\nLANG=\"tr_TR.UTF-8\"\n".to_owned());
        assert_eq!(
            host_languages_from(env(&[]), &file).as_deref(),
            Some("tr-TR,tr")
        );
        // C means no language was chosen.
        let c = env(&[("LANG", "C.UTF-8")]);
        assert_eq!(host_languages_from(c, &file), None);
        assert_eq!(host_languages_from(env(&[]), &none), None);
    }

    #[test]
    fn the_host_time_zone_is_read_from_the_usual_places_and_only_safe_names_pass() {
        let link = std::path::Path::new("/usr/share/zoneinfo/Europe/Istanbul");
        // TZ is already set: the browser inherits it, nothing to add.
        assert_eq!(
            host_timezone_from(Some("UTC"), Some("Europe/Berlin"), Some(link)),
            None
        );
        // /etc/timezone first (Debian), then the /etc/localtime link.
        assert_eq!(
            host_timezone_from(None, Some("Europe/Berlin\n"), Some(link)).as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            host_timezone_from(Some(" "), None, Some(link)).as_deref(),
            Some("Europe/Istanbul")
        );
        assert_eq!(
            host_timezone_from(
                None,
                None,
                Some(std::path::Path::new("../usr/share/zoneinfo/posix/Etc/UTC"))
            )
            .as_deref(),
            Some("posix/Etc/UTC")
        );
        // Nothing usable, or something that is not a zone name.
        assert_eq!(host_timezone_from(None, None, None), None);
        assert_eq!(
            host_timezone_from(None, Some("../../etc/passwd"), None),
            None
        );
        assert_eq!(host_timezone_from(None, Some("/etc/passwd"), None), None);
        assert_eq!(host_timezone_from(None, Some("A b"), None), None);
        let bad = std::path::Path::new("/usr/share/zoneinfo/../../etc/shadow");
        assert_eq!(host_timezone_from(None, None, Some(bad)), None);
    }

    #[test]
    fn display_probe_times_out_and_a_failure_is_retried_later() {
        let started = std::time::Instant::now();
        assert!(!bounded_probe(std::time::Duration::from_millis(10), || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            true
        }));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));

        let mut cache = ProbeCache::new();
        let t0 = std::time::Instant::now();
        assert!(!cache.get(t0, || false));
        // Inside the window the failure is trusted: no new probe.
        assert!(
            !cache.get(t0 + std::time::Duration::from_secs(1), || panic!(
                "must not re-probe yet"
            ))
        );
        // After it, a display that came up late is picked up and then sticks.
        assert!(cache.get(t0 + RETRY_AFTER, || true));
        assert!(cache.get(t0 + RETRY_AFTER * 9, || panic!("reachable is cached")));
    }
    #[test]
    fn session_zero_station_is_not_a_display() {
        assert!(!station_visible(0));
        assert!(station_visible(1));
    }
    #[test]
    fn display_environment_permutations() {
        for display in [None, Some(""), Some("  ")] {
            assert!(!display_configured(display));
        }
        for display in [Some(":99"), Some(":0.0"), Some("localhost:10.0")] {
            assert!(display_configured(display));
        }
        // WAYLAND_DISPLAY alone never substitutes for DISPLAY.
        assert!(!display_configured(None));
    }
    #[test]
    fn ram_threshold() {
        assert_eq!(default_computer_runs(0), 1);
        assert_eq!(default_computer_runs(8 * 1024 * 1024 * 1024 - 1), 1);
        assert_eq!(default_computer_runs(8 * 1024 * 1024 * 1024), 2);
    }
}
