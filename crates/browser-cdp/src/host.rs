//! Small host probes. These never launch a browser.

/// Whether DISPLAY names an X11 display (including Xwayland/Xvfb).
#[must_use]
pub fn display_configured(display: Option<&str>) -> bool {
    display.is_some_and(|s| !s.trim().is_empty())
}

/// Session reachability. The X11 handshake runs on a dedicated OS thread, never
/// on a Tokio worker, bounded to 1 s. A reachable display is remembered for the
/// process life; an unreachable one is probed again after [`RETRY_AFTER`], so a
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
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("display-probe".into())
        .spawn(move || {
            let _ = sender.send(probe());
        })
        .is_err()
    {
        return false;
    }
    receiver.recv_timeout(timeout).unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
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
