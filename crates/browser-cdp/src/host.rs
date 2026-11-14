//! Small host probes. These never launch a browser.

/// Whether DISPLAY names an X11 display (including Xwayland/Xvfb).
#[must_use]
pub fn display_configured(display: Option<&str>) -> bool {
    display.is_some_and(|s| !s.trim().is_empty())
}

/// Reachability includes Xauthority validation on Linux.
#[must_use]
pub fn has_display() -> bool {
    #[cfg(target_os = "linux")]
    {
        display_configured(std::env::var("DISPLAY").ok().as_deref()) && x11rb::connect(None).is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
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
