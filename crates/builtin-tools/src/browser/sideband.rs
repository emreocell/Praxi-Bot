//! What a browser call hands to the agent loop OUTSIDE its text.
//!
//! The loop must not learn two things by reading the words a page could also have written:
//! which picture a step saved (a page can print `screenshot: <path>` too), and what kind of
//! content a FAILED call carries (an error is a `String`, it has no exposure field). The tool
//! notes both here, per run, as structured facts; the loop takes them right after the call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tool_registry::OutputExposure;

/// Runs remembered at once; more means something is not taking them, so they are dropped.
const MAX_RUNS: usize = 64;

/// The facts of one call.
#[derive(Debug, Default, Clone)]
pub struct Sideband {
    /// The picture the call saved under the data directory, if any.
    pub screenshot: Option<PathBuf>,
    /// What the call's error is made of, when it failed after reading a page.
    pub exposure: Option<OutputExposure>,
}

fn map() -> std::sync::MutexGuard<'static, HashMap<String, Sideband>> {
    static MAP: OnceLock<Mutex<HashMap<String, Sideband>>> = OnceLock::new();
    MAP.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn entry<'a>(m: &'a mut HashMap<String, Sideband>, run_id: &str) -> &'a mut Sideband {
    if m.len() >= MAX_RUNS && !m.contains_key(run_id) {
        m.clear();
    }
    m.entry(run_id.to_owned()).or_default()
}

/// Record the picture a call of `run_id` saved (the newest wins).
pub fn note_screenshot(run_id: &str, path: &Path) {
    entry(&mut map(), run_id).screenshot = Some(path.to_path_buf());
}

/// Record what the failing call of `run_id` carries.
pub(super) fn note_exposure(run_id: &str, exposure: OutputExposure) {
    entry(&mut map(), run_id).exposure = Some(exposure);
}

/// Take (and clear) what the calls of `run_id` noted since the last take.
#[must_use]
pub fn take(run_id: &str) -> Sideband {
    map().remove(run_id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_are_per_run_and_taken_once() {
        note_screenshot("run-a", Path::new("/data/screenshots/a.jpg"));
        note_exposure(
            "run-a",
            OutputExposure {
                ingests_untrusted: true,
                reads_private: true,
                label: "signed-in browser".into(),
                trust_folder: None,
            },
        );
        assert!(take("run-b").screenshot.is_none());
        let a = take("run-a");
        assert_eq!(
            a.screenshot.as_deref(),
            Some(Path::new("/data/screenshots/a.jpg"))
        );
        assert!(a.exposure.is_some_and(|e| e.reads_private));
        let again = take("run-a");
        assert!(again.screenshot.is_none() && again.exposure.is_none());
    }
}
