//! Defence in depth for the custom Tauri commands.
//!
//! The capability files decide which window label may call which command; this
//! module repeats the decision inside the highest-impact commands, so one
//! mistaken capability edit (or a future wildcard) cannot hand them to the
//! floating widget or an overlay window.

/// The only window that may call the commands that start, finish or configure
/// runs, spawn processes, hand out the daemon token or change OS state.
pub const MAIN_ONLY: &[&str] = &["main"];

/// The windows that show the Stop control (main window and floating widget).
pub const MAIN_AND_WIDGET: &[&str] = &["main", "widget"];

/// The Quick ask window: it may read the daemon connection, hide itself and hand a chat to the
/// main window, nothing else.
pub const QUICK_ASK_ONLY: &[&str] = &["quick-ask"];

/// Whether `label` is one of `allowed` (exact match; no patterns here, the
/// capability files already use globs for the dynamically labelled overlays).
#[must_use]
pub fn label_allowed(label: &str, allowed: &[&str]) -> bool {
    allowed.contains(&label)
}

/// Rejects a command call that does not come from one of `allowed`.
///
/// # Errors
/// When the calling window's label is not in `allowed`.
pub fn require_window<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    allowed: &[&str],
) -> Result<(), String> {
    check_label(window.label(), allowed)
}

fn check_label(label: &str, allowed: &[&str]) -> Result<(), String> {
    if label_allowed(label, allowed) {
        Ok(())
    } else {
        Err(format!("command not allowed from window \"{label}\""))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAIN_AND_WIDGET, MAIN_ONLY, check_label, label_allowed};

    #[test]
    fn only_listed_labels_pass() {
        assert!(label_allowed("main", MAIN_ONLY));
        assert!(!label_allowed("widget", MAIN_ONLY));
        assert!(!label_allowed("overlay-0", MAIN_ONLY));
        assert!(!label_allowed("", MAIN_ONLY));
        assert!(!label_allowed("main-evil", MAIN_ONLY));
        assert!(label_allowed("widget", MAIN_AND_WIDGET));
        assert!(!label_allowed("overlay-1", MAIN_AND_WIDGET));
    }

    #[test]
    fn a_denied_call_returns_an_error_naming_the_window() {
        assert_eq!(check_label("main", MAIN_ONLY), Ok(()));
        assert_eq!(
            check_label("widget", MAIN_ONLY),
            Err("command not allowed from window \"widget\"".to_owned())
        );
    }
}
