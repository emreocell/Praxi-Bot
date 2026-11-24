//! Calendars of `bot serve`: loads `<data dir>/calendar.toml` and registers the
//! `calendar_*` tools. Passwords live in the secret store only; the file holds
//! the reference (`password = "calendar/home/password"`), never the value.
//!
//! ```toml
//! [[calendar]]
//! name = "home"
//! url = "https://dav.example.com/calendars/me/personal/"
//! username = "me"
//! password = "calendar/home/password"
//! ```

use std::path::Path;
use std::sync::Arc;

use connector_calendar::{CalendarAccount, CalendarConnector};
use secret_store::SecretStore;
use serde::Deserialize;
use tool_registry::ToolRegistry;

/// Filename of the calendar configuration in `<data_dir>`.
pub const CALENDAR_FILE: &str = "calendar.toml";

#[derive(Debug, Default, Deserialize)]
struct CalendarConfigFile {
    #[serde(default, alias = "calendars")]
    calendar: Vec<CalendarAccount>,
}

/// Register the calendar tools (they stay hidden from bots while no calendar is
/// usable). Returns how many tools were added; `0` without a secret store.
pub fn start_calendar(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> usize {
    let Some(store) = store else {
        return 0;
    };
    let path = data_dir.join(CALENDAR_FILE);
    let accounts = match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<CalendarConfigFile>(&text) {
            Ok(cfg) => cfg.calendar,
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {}: {e}", path.display());
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: cannot read {}: {e}", path.display());
            Vec::new()
        }
    };
    let (connector, problems) = CalendarConnector::new(accounts, store);
    for p in problems {
        support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {p}");
    }
    let added = connector_calendar::register(registry, &Arc::new(connector));
    support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "calendar: {added} tool(s) registered");
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;

    #[test]
    fn calendar_toml_registers_the_tools_and_bad_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        std::fs::write(
            dir.path().join(CALENDAR_FILE),
            r#"
[[calendar]]
name = "home"
url = "https://dav.example.com/cal/"
username = "me"
password = "calendar/home/password"

[[calendar]]
name = "plain"
url = "http://dav.example.com/cal/"
username = "me"
password = "calendar/plain/password"
"#,
        )
        .unwrap();
        let registry = ToolRegistry::new();
        assert_eq!(start_calendar(dir.path(), &registry, Some(&store)), 3);
        assert!(registry.get("calendar_create_event").is_some());
    }
}
