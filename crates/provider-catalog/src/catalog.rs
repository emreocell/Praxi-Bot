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
    /// Load all `*.toml` files from `catalog_dir`.
    ///
    /// Files that fail to parse are skipped and their errors returned alongside
    /// the successfully-loaded entries.
    ///
    /// # Errors
    ///
    /// Returns a vec of errors for any files that could not be parsed.
    pub fn load_dir(catalog_dir: &Path) -> Result<Self, Vec<ProviderError>> {
        let mut entries = HashMap::new();
        let mut errors = Vec::new();

        let read_dir = match std::fs::read_dir(catalog_dir) {
            Ok(d) => d,
            Err(e) => {
                errors.push(ProviderError::Connection(format!(
                    "cannot read catalog directory {}: {e}",
                    catalog_dir.display()
                )));
                return Err(errors);
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

        if errors.is_empty() {
            Ok(Self { entries })
        } else {
            // Return partial results alongside errors.
            Err(errors)
        }
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
}
