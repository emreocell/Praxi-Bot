//! Catalog loader: reads all `providers/catalog/*.toml` files.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{entry::ProviderEntry, error::ProviderError};

/// An in-memory catalog of provider entries, keyed by `id`.
#[derive(Debug, Default, Clone)]
pub struct Catalog {
    entries: HashMap<String, ProviderEntry>,
}

impl Catalog {
    /// Load all `*.toml` files from `catalog_dir`, fail-fast: if any file
    /// cannot be read or parsed, no catalog is returned and every error is
    /// reported. Use [`Catalog::load_dir_lenient`] to keep the valid entries.
    ///
    /// # Errors
    ///
    /// Returns a vec of errors for any files that could not be read or parsed
    /// (or the directory itself).
    pub fn load_dir(catalog_dir: &Path) -> Result<Self, Vec<ProviderError>> {
        let (catalog, errors) = Self::load_dir_lenient(catalog_dir);
        if errors.is_empty() {
            Ok(catalog)
        } else {
            Err(errors)
        }
    }

    /// Load all `*.toml` files from `catalog_dir`, skipping files that fail to
    /// read or parse. Returns the catalog of every valid entry together with
    /// one error per skipped file (or for an unreadable directory).
    #[must_use]
    pub fn load_dir_lenient(catalog_dir: &Path) -> (Self, Vec<ProviderError>) {
        let mut entries = HashMap::new();
        let mut errors = Vec::new();

        let read_dir = match std::fs::read_dir(catalog_dir) {
            Ok(d) => d,
            Err(e) => {
                errors.push(ProviderError::Connection(format!(
                    "cannot read catalog directory {}: {e}",
                    catalog_dir.display()
                )));
                return (Self::default(), errors);
            }
        };

        for entry in read_dir.flatten() {
            let path: PathBuf = entry.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    errors.push(ProviderError::Connection(format!(
                        "cannot read {}: {e}",
                        path.display()
                    )));
                    continue;
                }
            };
            match toml::from_str::<ProviderEntry>(&content) {
                Ok(p) => {
                    entries.insert(p.id.clone(), p);
                }
                Err(e) => {
                    errors.push(ProviderError::CatalogParse {
                        path: path.display().to_string(),
                        source: e,
                    });
                }
            }
        }

        (Self { entries }, errors)
    }

    /// Build a catalog from an explicit list of entries (useful in tests).
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = ProviderEntry>) -> Self {
        Self {
            entries: entries.into_iter().map(|e| (e.id.clone(), e)).collect(),
        }
    }

    /// Look up a provider by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ProviderEntry> {
        self.entries.get(id)
    }

    /// Return all entries.
    pub fn all(&self) -> impl Iterator<Item = &ProviderEntry> {
        self.entries.values()
    }

    /// Number of entries in the catalog.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if the catalog has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Auth, CompatFlags, ModelCapabilities, ProviderEntry, ProviderKind};

    fn sample() -> ProviderEntry {
        ProviderEntry {
            id: "test-provider".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".into(),
            auth: Auth::ApiKeyEnv("TEST_API_KEY".into()),
            capabilities: ModelCapabilities {
                tools: true,
                vision: false,
                streaming: true,
                json_mode: true,
            },
            compat_flags: CompatFlags::default(),
            models_endpoint: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn from_entries_and_get() {
        let cat = Catalog::from_entries([sample()]);
        assert_eq!(cat.len(), 1);
        assert!(cat.get("test-provider").is_some());
        assert!(cat.get("missing").is_none());
    }

    #[test]
    fn load_dir_lenient_keeps_valid_entries() {
        let dir = std::env::temp_dir().join(format!(
            "pc-catalog-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.toml"), toml::to_string(&sample()).unwrap()).unwrap();
        std::fs::write(dir.join("bad.toml"), "this is = not [valid").unwrap();

        let (cat, errs) = Catalog::load_dir_lenient(&dir);
        assert_eq!(cat.len(), 1);
        assert!(cat.get("test-provider").is_some());
        assert_eq!(errs.len(), 1);

        // fail-fast variant reports the error and no catalog
        assert_eq!(Catalog::load_dir(&dir).unwrap_err().len(), 1);

        let (cat, errs) = Catalog::load_dir_lenient(&dir.join("missing"));
        assert!(cat.is_empty());
        assert_eq!(errs.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shipped_catalog_toml_loads_without_capability_overrides() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/catalog");
        let cat = Catalog::load_dir(&dir).expect("every catalog toml parses");
        for id in [
            "ollama",
            "lmstudio",
            "openrouter",
            "anthropic",
            "gemini",
            "deepseek",
            "gemini-openai-compat",
        ] {
            assert!(cat.get(id).is_some(), "{id} missing");
        }
        for e in cat.all() {
            for m in &e.models {
                assert!(
                    m.capabilities.is_none(),
                    "{} / {} should load with no capability override",
                    e.id,
                    m.id
                );
            }
        }
    }
}
