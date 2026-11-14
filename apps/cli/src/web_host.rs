//! Embedder backing `/v1/web/search*` (`daemon_api::WebControl`):
//! reads and atomically writes `<data_dir>/web.toml`, stores write-only API
//! keys (`web/brave`, `web/tavily`) in `SecretStore`, hot-reloads `web_*`
//! tools into `ToolRegistry`, and runs live search probes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use daemon_api::web::{
    WebControl, WebControlError, WebSearchConfigDto, WebSearchConfigInput, WebSearchTestInput,
    WebSearchTestReport,
};
use secret_store::SecretStore;
use tool_registry::ToolRegistry;
use web_tools::{HttpFetcher, KNOWN_BACKENDS, SearchQuery, TOOL_NAMES, WebToolsConfig};

use crate::serve::{
    WEB_FILE, WebConfigFile, daemon_protected_sockets, load_web_file, web_tools_config,
};

const DEFAULT_BRAVE_KEY_NAME: &str = "web/brave";
const DEFAULT_TAVILY_KEY_NAME: &str = "web/tavily";
const DEFAULT_TEST_QUERY: &str = "rust programming";
const FILE_HEADER: &str = "# Web search & HTTP configuration — edited via Web Search settings or manually.\n# API keys live in the secret store; only secret names belong here.\n\n";

/// Manages `web.toml`, web search API keys in `SecretStore`, and live
/// reloading of `web_*` tools inside `ToolRegistry`.
pub struct WebHost {
    data_dir: PathBuf,
    daemon_port: u16,
    registry: ToolRegistry,
    secrets: Option<Arc<dyn SecretStore>>,
    update_lock: tokio::sync::Mutex<()>,
}

impl WebHost {
    /// Construct a host controller bound to `data_dir` and the live `registry`.
    #[must_use]
    pub fn new(
        data_dir: &Path,
        daemon_port: u16,
        registry: ToolRegistry,
        secrets: Option<Arc<dyn SecretStore>>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            daemon_port,
            registry,
            secrets,
            update_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn secret_present(&self, name: Option<&str>) -> bool {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
            return false;
        };
        let Some(store) = &self.secrets else {
            return false;
        };
        store.get(name).ok().flatten().is_some()
    }

    fn effective_order(file: &WebConfigFile) -> Vec<String> {
        let mut out = Vec::with_capacity(KNOWN_BACKENDS.len());
        let mut seen = HashSet::new();
        if let Some(raw) = &file.search.order {
            for name in raw {
                if KNOWN_BACKENDS.contains(&name.as_str()) && seen.insert(name.clone()) {
                    out.push(name.clone());
                }
            }
        }
        for name in KNOWN_BACKENDS {
            if seen.insert((*name).to_owned()) {
                out.push((*name).to_owned());
            }
        }
        out
    }

    fn build_tools_cfg(&self, file: &WebConfigFile, warnings: &mut Vec<String>) -> WebToolsConfig {
        let protected = daemon_protected_sockets(self.daemon_port);
        web_tools_config(
            &self.data_dir,
            file,
            self.secrets.as_ref(),
            &protected,
            warnings,
        )
    }

    fn dto_from(&self, file: &WebConfigFile, extra_warnings: Vec<String>) -> WebSearchConfigDto {
        let mut warnings = extra_warnings;
        let cfg = self.build_tools_cfg(file, &mut warnings);
        let brave_key_set = self.secret_present(file.search.brave_key.as_deref());
        let tavily_key_set = self.secret_present(file.search.tavily_key.as_deref());
        let searxng_url = cfg.search.searxng_url.clone();
        let duckduckgo = cfg.search.duckduckgo;
        let duckduckgo_lite = cfg.search.duckduckgo_lite;
        let bing = cfg.search.bing;
        let order = Self::effective_order(file);

        let mut active_backends = Vec::new();
        for name in &order {
            let active = match name.as_str() {
                "searxng" => searxng_url.is_some(),
                "brave" => brave_key_set && cfg.search.brave_key.is_some(),
                "tavily" => tavily_key_set && cfg.search.tavily_key.is_some(),
                "duckduckgo" => duckduckgo,
                "duckduckgo_lite" => duckduckgo_lite,
                "bing" => bing,
                _ => false,
            };
            if active {
                active_backends.push(name.clone());
            }
        }

        let config_error = (!warnings.is_empty()).then(|| warnings.join("; "));

        WebSearchConfigDto {
            order,
            active_backends,
            searxng_url,
            brave_key_set,
            tavily_key_set,
            duckduckgo,
            duckduckgo_lite,
            bing,
            config_error,
        }
    }

    fn persist(&self, file: &WebConfigFile) -> Result<(), WebControlError> {
        let path = self.data_dir.join(WEB_FILE);
        let body = toml::to_string_pretty(file)
            .map_err(|e| WebControlError::Internal(format!("cannot serialize {WEB_FILE}: {e}")))?;
        let tmp = path.with_extension("toml.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(FILE_HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            WebControlError::Internal(format!("cannot save {}: {e}", path.display()))
        })
    }

    async fn store_secret(&self, name: String, value: String) -> Result<(), WebControlError> {
        let Some(store) = self.secrets.as_ref().map(Arc::clone) else {
            return Err(WebControlError::Invalid(
                "secret store is not available to store API keys".into(),
            ));
        };
        tokio::task::spawn_blocking(move || store.set(&name, &value))
            .await
            .map_err(|e| WebControlError::Internal(format!("secret task failed: {e}")))?
            .map_err(|e| WebControlError::Internal(format!("cannot store secret: {e}")))
    }

    async fn delete_secret(&self, name: String) -> Result<(), WebControlError> {
        let Some(store) = self.secrets.as_ref().map(Arc::clone) else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            let _ = store.delete(&name);
        })
        .await
        .map_err(|e| WebControlError::Internal(format!("secret task failed: {e}")))
    }
}

fn validate_order(raw: &[String]) -> Result<Vec<String>, WebControlError> {
    let mut out = Vec::with_capacity(KNOWN_BACKENDS.len());
    let mut seen = HashSet::new();
    for name in raw {
        if !KNOWN_BACKENDS.contains(&name.as_str()) {
            return Err(WebControlError::Invalid(format!(
                "unknown search backend `{name}` (known: {})",
                KNOWN_BACKENDS.join(", ")
            )));
        }
        if seen.insert(name.clone()) {
            out.push(name.clone());
        }
    }
    for name in KNOWN_BACKENDS {
        if seen.insert((*name).to_owned()) {
            out.push((*name).to_owned());
        }
    }
    Ok(out)
}

enum SecretChange {
    Keep,
    Set { name: String, value: String },
    Delete { name: String },
}

fn plan_secret_change(
    slot: &mut Option<String>,
    default_name: &str,
    new_value: Option<String>,
    clear: Option<bool>,
) -> SecretChange {
    if clear == Some(true) {
        return slot
            .take()
            .map_or(SecretChange::Keep, |name| SecretChange::Delete { name });
    }
    let Some(value) = new_value else {
        return SecretChange::Keep;
    };
    let name = slot
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| default_name.to_owned());
    *slot = Some(name.clone());
    SecretChange::Set { name, value }
}

fn validate_searxng_url(raw: &str) -> Result<Option<String>, WebControlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let u = url::Url::parse(trimmed)
        .map_err(|e| WebControlError::Invalid(format!("invalid searxng_url: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(WebControlError::Invalid(
            "searxng_url must use http:// or https://".into(),
        ));
    }
    if u.host_str().is_none() {
        return Err(WebControlError::Invalid(
            "searxng_url must include a host".into(),
        ));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(WebControlError::Invalid(
            "searxng_url must not contain credentials".into(),
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

#[async_trait]
impl WebControl for WebHost {
    fn get(&self) -> WebSearchConfigDto {
        let (file, warnings) = load_web_file(&self.data_dir);
        self.dto_from(&file, warnings)
    }

    async fn update(
        &self,
        input: WebSearchConfigInput,
    ) -> Result<WebSearchConfigDto, WebControlError> {
        if input.clear_brave_key == Some(true) && input.brave_key.is_some() {
            return Err(WebControlError::Invalid(
                "cannot both set and clear brave_key in the same request".into(),
            ));
        }
        if input.clear_tavily_key == Some(true) && input.tavily_key.is_some() {
            return Err(WebControlError::Invalid(
                "cannot both set and clear tavily_key in the same request".into(),
            ));
        }

        let _guard = self.update_lock.lock().await;
        // Start from defaults when web.toml is missing, unreadable, or malformed so
        // a valid settings save can repair/replace the broken file on disk.
        let (mut file, _warnings) = load_web_file(&self.data_dir);

        if let Some(order) = &input.order {
            file.search.order = Some(validate_order(order)?);
        }
        if let Some(url) = &input.searxng_url {
            file.search.searxng_url = validate_searxng_url(url)?;
        }
        if let Some(v) = input.duckduckgo {
            file.search.duckduckgo = Some(v);
        }
        if let Some(v) = input.duckduckgo_lite {
            file.search.duckduckgo_lite = Some(v);
        }
        if let Some(v) = input.bing {
            file.search.bing = Some(v);
        }

        let brave_change = plan_secret_change(
            &mut file.search.brave_key,
            DEFAULT_BRAVE_KEY_NAME,
            input.brave_key,
            input.clear_brave_key,
        );
        let tavily_change = plan_secret_change(
            &mut file.search.tavily_key,
            DEFAULT_TAVILY_KEY_NAME,
            input.tavily_key,
            input.clear_tavily_key,
        );

        if (matches!(brave_change, SecretChange::Set { .. })
            || matches!(tavily_change, SecretChange::Set { .. }))
            && self.secrets.is_none()
        {
            return Err(WebControlError::Invalid(
                "secret store is not available to store API keys".into(),
            ));
        }

        // Validate candidate configuration (SSRF / URL / HttpFetcher construction)
        // before mutating secrets, disk, or live tools.
        let mut warnings = Vec::new();
        let candidate_cfg = self.build_tools_cfg(&file, &mut warnings);
        if let Some(w) = warnings.first()
            && w.contains("searxng")
        {
            return Err(WebControlError::Invalid(w.clone()));
        }
        web_tools::tools(candidate_cfg.clone())
            .map_err(|e| WebControlError::Invalid(e.to_string()))?;

        for change in [brave_change, tavily_change] {
            match change {
                SecretChange::Keep => {}
                SecretChange::Set { name, value } => self.store_secret(name, value).await?,
                SecretChange::Delete { name } => self.delete_secret(name).await?,
            }
        }

        self.persist(&file)?;

        for name in TOOL_NAMES {
            self.registry.unregister(name);
        }
        builtin_tools::register_web_tools(&self.registry, candidate_cfg)
            .map_err(|e| WebControlError::Internal(format!("cannot reload web tools: {e}")))?;

        Ok(self.dto_from(&file, Vec::new()))
    }

    async fn test(
        &self,
        input: WebSearchTestInput,
    ) -> Result<WebSearchTestReport, WebControlError> {
        let query = input
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .unwrap_or(DEFAULT_TEST_QUERY)
            .to_owned();
        let backend_filter = input
            .backend
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_owned);
        if let Some(b) = &backend_filter
            && !KNOWN_BACKENDS.contains(&b.as_str())
        {
            return Err(WebControlError::Invalid(format!(
                "unknown search backend `{b}` (known: {})",
                KNOWN_BACKENDS.join(", ")
            )));
        }

        let (file, _warnings) = load_web_file(&self.data_dir);
        let mut warnings = Vec::new();
        let cfg = self.build_tools_cfg(&file, &mut warnings);
        let fetcher = HttpFetcher::new(Arc::clone(&cfg.policy), cfg.http.clone())
            .map_err(|e| WebControlError::Internal(format!("cannot build http fetcher: {e}")))?;
        let mut search_cfg = cfg.search.clone();
        // Each test probe should actively hit enabled backends without being masked by cooldowns.
        search_cfg.cooldown = std::time::Duration::ZERO;
        let search = search_cfg
            .build(&fetcher, &cfg.http, cfg.secrets.clone())
            .map_err(|e| WebControlError::Invalid(e.to_string()))?;

        let started = Instant::now();
        let res = search
            .search(&SearchQuery::new(&query), backend_filter.as_deref())
            .await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        match res {
            Ok(outcome) => Ok(WebSearchTestReport {
                ok: true,
                query,
                backend: Some(outcome.backend),
                result_count: outcome.results.len(),
                elapsed_ms,
                error: None,
                fallback_errors: outcome.fallback_errors,
            }),
            Err(e) => Ok(WebSearchTestReport {
                ok: false,
                query,
                backend: None,
                result_count: 0,
                elapsed_ms,
                error: Some(e.to_string()),
                fallback_errors: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn host_with_store(dir: &Path) -> (WebHost, Arc<dyn SecretStore>, ToolRegistry) {
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let reg = ToolRegistry::new();
        crate::serve::register_web(dir, 9999, &reg, Some(&store));
        let host = WebHost::new(dir, 9999, reg.clone(), Some(Arc::clone(&store)));
        (host, store, reg)
    }

    #[test]
    fn get_returns_default_order_and_active_keyless_chain() {
        let dir = tempfile::tempdir().unwrap();
        let (host, _, _) = host_with_store(dir.path());
        let dto = host.get();
        assert_eq!(
            dto.order,
            vec![
                "searxng",
                "brave",
                "tavily",
                "duckduckgo",
                "duckduckgo_lite",
                "bing"
            ]
        );
        assert_eq!(
            dto.active_backends,
            vec!["duckduckgo", "duckduckgo_lite", "bing"]
        );
        assert!(!dto.brave_key_set);
        assert!(!dto.tavily_key_set);
        assert!(dto.duckduckgo);
        assert!(dto.duckduckgo_lite);
        assert!(dto.bing);
    }

    #[tokio::test]
    async fn update_stores_keys_write_only_and_hot_reloads_tool_schema() {
        let dir = tempfile::tempdir().unwrap();
        let (host, store, reg) = host_with_store(dir.path());
        let updated = host
            .update(WebSearchConfigInput {
                order: Some(vec!["bing".into(), "tavily".into(), "duckduckgo".into()]),
                brave_key: Some("BSA-secret-123".into()),
                tavily_key: Some("tvly-secret-456".into()),
                duckduckgo_lite: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(updated.brave_key_set);
        assert!(updated.tavily_key_set);
        assert!(!updated.duckduckgo_lite);
        assert_eq!(
            updated.active_backends,
            vec!["bing", "tavily", "duckduckgo", "brave"]
        );
        assert_eq!(
            store.get("web/brave").unwrap().unwrap().expose(),
            "BSA-secret-123"
        );
        assert_eq!(
            store.get("web/tavily").unwrap().unwrap().expose(),
            "tvly-secret-456"
        );

        let disk = std::fs::read_to_string(dir.path().join(WEB_FILE)).unwrap();
        assert!(!disk.contains("BSA-secret-123"));
        assert!(!disk.contains("tvly-secret-456"));
        assert!(disk.contains("brave_key = \"web/brave\""));

        let tool = reg.get("web_search").expect("web_search registered");
        let enum_vals = tool.spec().input_schema["properties"]["backend"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(enum_vals, vec!["bing", "tavily", "duckduckgo", "brave"]);

        let cleared = host
            .update(WebSearchConfigInput {
                clear_brave_key: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!cleared.brave_key_set);
        assert!(store.get("web/brave").unwrap().is_none());
    }

    #[tokio::test]
    async fn update_rejects_unknown_backend_and_credentials_in_searxng_url() {
        let dir = tempfile::tempdir().unwrap();
        let (host, _, _) = host_with_store(dir.path());
        let err = host
            .update(WebSearchConfigInput {
                order: Some(vec!["bogus".into()]),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, WebControlError::Invalid(_)));

        let err = host
            .update(WebSearchConfigInput {
                searxng_url: Some("http://user:pass@localhost:8080".into()),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, WebControlError::Invalid(_)));
    }

    #[tokio::test]
    async fn test_probe_uses_configured_searxng_and_reports_elapsed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    {"title": "Rust", "url": "https://www.rust-lang.org", "content": "Systems lang"}
                ]
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let (host, _, _) = host_with_store(dir.path());
        host.update(WebSearchConfigInput {
            searxng_url: Some(server.uri()),
            duckduckgo: Some(false),
            duckduckgo_lite: Some(false),
            bing: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();

        let report = host
            .test(WebSearchTestInput {
                query: Some("rust".into()),
                backend: None,
            })
            .await
            .unwrap();
        assert!(report.ok);
        assert_eq!(report.backend.as_deref(), Some("searxng"));
        assert_eq!(report.result_count, 1);
        assert!(report.error.is_none());
    }

    #[tokio::test]
    async fn update_rejects_searxng_on_daemon_port_via_localhost_without_connecting() {
        let daemon = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&daemon)
            .await;
        let daemon_port = daemon.address().port();

        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let reg = ToolRegistry::new();
        crate::serve::register_web(dir.path(), daemon_port, &reg, Some(&store));
        let host = WebHost::new(dir.path(), daemon_port, reg, Some(store));

        for url in [
            format!("http://localhost:{daemon_port}/v1/web"),
            format!("http://127.0.0.1:{daemon_port}/v1/web"),
        ] {
            let err = host
                .update(WebSearchConfigInput {
                    searxng_url: Some(url.clone()),
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert!(matches!(err, WebControlError::Invalid(_)), "{url}: {err:?}");
        }
        assert!(!dir.path().join(WEB_FILE).exists());
        assert!(
            daemon.received_requests().await.unwrap().is_empty(),
            "daemon port must never be contacted"
        );
    }

    #[tokio::test]
    async fn get_surfaces_broken_web_toml_and_update_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(WEB_FILE), "this is = = broken toml").unwrap();
        let (host, _, _) = host_with_store(dir.path());

        let dto = host.get();
        let err = dto
            .config_error
            .as_deref()
            .expect("get must surface broken web.toml parse error");
        assert!(err.contains(WEB_FILE), "{err}");
        assert_eq!(
            dto.active_backends,
            vec!["duckduckgo", "duckduckgo_lite", "bing"]
        );

        let updated = host
            .update(WebSearchConfigInput {
                bing: Some(false),
                ..Default::default()
            })
            .await
            .expect("valid settings save must replace broken web.toml");
        assert!(updated.config_error.is_none(), "{:?}", updated.config_error);
        assert!(!updated.bing);
        assert_eq!(
            updated.active_backends,
            vec!["duckduckgo", "duckduckgo_lite"]
        );

        let after = host.get();
        assert!(after.config_error.is_none());
        assert!(!after.bing);
        let disk = std::fs::read_to_string(dir.path().join(WEB_FILE)).unwrap();
        assert!(toml::from_str::<WebConfigFile>(&disk).is_ok());
    }
}
