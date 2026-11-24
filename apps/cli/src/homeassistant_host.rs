//! Home Assistant of `bot serve`: loads `<data dir>/homeassistant.toml` and
//! registers the `ha_*` tools. The access token lives in the secret store only.
//!
//! ```toml
//! url = "http://homeassistant.local:8123"
//! token = "homeassistant/token"
//! allowed_domains = ["light", "switch", "climate"]   # optional
//! ```

use std::path::Path;
use std::sync::Arc;

use connector_homeassistant::HomeAssistantConfig;
use secret_store::SecretStore;
use tool_registry::ToolRegistry;

/// Filename of the Home Assistant configuration in `<data_dir>`.
pub const HOMEASSISTANT_FILE: &str = "homeassistant.toml";

fn load(data_dir: &Path) -> Option<HomeAssistantConfig> {
    let path = data_dir.join(HOMEASSISTANT_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: {}: {e}", path.display());
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: cannot read {}: {e}", path.display());
            None
        }
    }
}

/// Register the Home Assistant tools (hidden from bots while the file is
/// missing or refused). Returns how many were added; `0` without a secret store.
pub fn start_homeassistant(
    data_dir: &Path,
    registry: &ToolRegistry,
    store: Option<&Arc<dyn SecretStore>>,
) -> usize {
    let Some(store) = store else {
        return 0;
    };
    let (added, problem) = connector_homeassistant::register(registry, load(data_dir), store);
    if let Some(p) = problem {
        support_logs::tracing::warn!(target: "praxi_support", reason = "component_warning", "warning: home assistant: {p}");
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;

    #[test]
    fn homeassistant_toml_registers_the_tools() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        std::fs::write(
            dir.path().join(HOMEASSISTANT_FILE),
            "url = \"http://192.168.1.10:8123\"\ntoken = \"homeassistant/token\"\nallowed_domains = [\"light\"]\n",
        )
        .unwrap();
        let registry = ToolRegistry::new();
        assert_eq!(start_homeassistant(dir.path(), &registry, Some(&store)), 3);
        assert!(registry.get("ha_call_service").is_some());
    }
}
