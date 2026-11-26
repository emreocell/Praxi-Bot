//! Composio app connections of `bot serve`: plugs the `composio` crate into the
//! daemon's `/v1/composio/*` routes ([`daemon_api::ComposioControl`]) and the
//! tool registry.  The API key lives in the secret store under the reserved
//! `composio/` prefix; nothing here ever returns, logs or audits it.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use composio::{
    AppView, Audit, CatalogSource, ComposioError, ComposioService, ConnState, KEY_SETUP_URL,
    Linkage, LocalKeySource, LoginState, Redact, ServiceConfig, Toolkit,
};
use daemon_api::composio::{
    ComposioActionDto, ComposioAppDto, ComposioCatalogDto, ComposioCatalogQuery, ComposioConnState,
    ComposioConnectDto, ComposioDisconnectDto, ComposioLinkMode, ComposioLoginDto,
    ComposioLoginState, ComposioLoginStatusDto, ComposioStatusDto, ComposioTestDto,
    ComposioToolkitDto,
};
use daemon_api::{ComposioControl, ComposioControlError};
use secret_store::SecretStore;
use tool_registry::ToolRegistry;

const DEFAULT_PAGE: u32 = 48;

/// The Composio service behind the daemon API.
pub struct ComposioHost {
    svc: ComposioService,
}

/// Audit rows of the tools and the connect flow, written through the daemon.
pub struct DaemonAudit<F>(pub F);

impl<F> Audit for DaemonAudit<F>
where
    F: Fn(&str, &serde_json::Value) -> Result<(), String> + Send + Sync,
{
    fn record(&self, action: &str, detail: serde_json::Value) -> Result<(), String> {
        (self.0)(action, &detail)
    }
}

impl ComposioHost {
    /// Load `composio.toml` and register the search/run tools.  Connected apps
    /// are refreshed in the background.  Must be called inside a tokio runtime.
    ///
    /// # Errors
    /// `composio.toml` exists but cannot be parsed.
    pub fn start(
        data_dir: &Path,
        registry: ToolRegistry,
        secrets: Arc<dyn SecretStore>,
        redact: Redact,
        audit: Arc<dyn Audit>,
        register_secret: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Arc<Self>, String> {
        let svc = ComposioService::new(ServiceConfig {
            data_dir: data_dir.to_path_buf(),
            keys: Arc::new(LocalKeySource::new(secrets)),
            registry,
            redact,
            register_secret,
            audit,
            loopback_base: None,
        })
        .map_err(|e| e.to_string())?;
        let bg = svc.clone();
        tokio::spawn(async move {
            bg.refresh_all().await;
            bg.spawn_catalog_refresh();
        });
        Ok(Arc::new(Self { svc }))
    }

    fn app_dto(v: AppView) -> ComposioAppDto {
        ComposioAppDto {
            slug: v.slug,
            name: v.name,
            state: conn(v.state),
            enabled: v.enabled,
            native_first: v.native_first,
            bots: v.bots,
            tools: u32::try_from(v.tools).unwrap_or(u32::MAX),
            account: v.account,
        }
    }

    fn toolkit_dto(&self, t: &Toolkit, group: &str, apps: &[AppView]) -> ComposioToolkitDto {
        let mine = apps.iter().find(|a| a.slug == t.slug);
        ComposioToolkitDto {
            slug: t.slug.clone(),
            name: t.name.clone(),
            description: t.description.clone(),
            categories: t.categories.clone(),
            tools_count: t.tools_count,
            group: group.to_owned(),
            logo: t.logo.clone(),
            native_first: self.svc.curated().is_native_first(&t.slug),
            state: mine.map_or(ComposioConnState::NotConnected, |a| conn(a.state)),
            enabled: mine.is_some_and(|a| a.enabled),
        }
    }
}

fn conn(s: ConnState) -> ComposioConnState {
    match s {
        ConnState::NotConnected => ComposioConnState::NotConnected,
        ConnState::Pending => ComposioConnState::Pending,
        ConnState::Active => ComposioConnState::Active,
        ConnState::Expired => ComposioConnState::Expired,
        ConnState::Failed => ComposioConnState::Failed,
    }
}

fn map_err(e: ComposioError) -> ComposioControlError {
    match e {
        ComposioError::NotConfigured => ComposioControlError::NotConfigured,
        ComposioError::Invalid(m) => ComposioControlError::Invalid(m),
        ComposioError::NotFound(m) => ComposioControlError::NotFound(m),
        e @ ComposioError::RateLimited { .. } => ComposioControlError::RateLimited(e.to_string()),
        ComposioError::Unauthorized => ComposioControlError::LoginExpired,
        e @ (ComposioError::Upstream(_) | ComposioError::Transport(_)) => {
            ComposioControlError::Upstream(e.to_string())
        }
        ComposioError::Internal(m) => ComposioControlError::Internal(m),
    }
}

#[async_trait]
impl ComposioControl for ComposioHost {
    async fn status(&self) -> ComposioStatusDto {
        let account = self.svc.account_label().await;
        ComposioStatusDto {
            configured: self.svc.configured(),
            mode: match self.svc.linkage() {
                Linkage::None => ComposioLinkMode::None,
                Linkage::Login => ComposioLinkMode::Login,
                Linkage::ProjectKey => ComposioLinkMode::ProjectKey,
            },
            key_setup_url: KEY_SETUP_URL.to_owned(),
            login_expired: self.svc.login_expired(),
            account,
        }
    }

    async fn login_start(&self) -> Result<ComposioLoginDto, ComposioControlError> {
        let s = self.svc.login_start().await.map_err(map_err)?;
        Ok(ComposioLoginDto {
            login_url: s.login_url,
            expires_at: s.expires_at,
        })
    }

    async fn login_poll(&self) -> Result<ComposioLoginStatusDto, ComposioControlError> {
        let state = match self.svc.login_poll().await.map_err(map_err)? {
            LoginState::Idle => ComposioLoginState::Idle,
            LoginState::Pending => ComposioLoginState::Pending,
            LoginState::Linked => ComposioLoginState::Linked,
            LoginState::Failed => ComposioLoginState::Failed,
        };
        Ok(ComposioLoginStatusDto { state })
    }

    async fn set_key(&self, key: &str) -> Result<(), ComposioControlError> {
        self.svc.set_key(key).await.map_err(map_err)
    }

    async fn remove_key(&self) -> Result<(), ComposioControlError> {
        self.svc.remove_key().await.map_err(map_err)
    }

    async fn catalog(
        &self,
        q: ComposioCatalogQuery,
    ) -> Result<ComposioCatalogDto, ComposioControlError> {
        // Memory only: never waits for Composio; a stale cache refreshes in
        // the background.
        let view = self.svc.catalog_view(
            q.search.as_deref(),
            q.group.as_deref(),
            q.cursor.as_deref(),
            q.limit.unwrap_or(DEFAULT_PAGE) as usize,
        );
        let apps = self.svc.apps();
        let items: Vec<ComposioToolkitDto> = view
            .items
            .iter()
            .map(|(t, g)| self.toolkit_dto(t, g, &apps))
            .collect();
        Ok(ComposioCatalogDto {
            items,
            next_cursor: view.next_cursor,
            source: match view.source {
                CatalogSource::Bundled => "bundled",
                CatalogSource::Cache => "cache",
            }
            .to_owned(),
            updated_at: view.updated_at,
            refreshing: self.svc.catalog_refreshing(),
            error: self.svc.catalog_error().map(str::to_owned),
        })
    }

    async fn logo(
        &self,
        slug: &str,
    ) -> Result<Option<daemon_api::composio::ComposioLogo>, ComposioControlError> {
        Ok(self.svc.logo(slug).await.map_err(map_err)?.map(|l| {
            daemon_api::composio::ComposioLogo {
                bytes: l.bytes,
                content_type: l.content_type,
            }
        }))
    }

    async fn actions(&self, slug: &str) -> Result<Vec<ComposioActionDto>, ComposioControlError> {
        Ok(self
            .svc
            .toolkit_actions(slug)
            .await
            .map_err(map_err)?
            .iter()
            .map(|action| ComposioActionDto {
                name: action.name.clone(),
                description: action.description.clone(),
            })
            .collect())
    }

    fn apps(&self) -> Vec<ComposioAppDto> {
        self.svc.apps().into_iter().map(Self::app_dto).collect()
    }

    async fn app(&self, slug: &str) -> Result<ComposioAppDto, ComposioControlError> {
        self.svc
            .app_status(slug)
            .await
            .map(Self::app_dto)
            .map_err(map_err)
    }

    async fn connect(&self, slug: &str) -> Result<ComposioConnectDto, ComposioControlError> {
        let link = self.svc.connect(slug).await.map_err(map_err)?;
        Ok(ComposioConnectDto {
            redirect_url: link.redirect_url,
            expires_at: link.expires_at,
        })
    }

    async fn set_enabled(
        &self,
        slug: &str,
        enabled: bool,
    ) -> Result<ComposioAppDto, ComposioControlError> {
        self.svc
            .set_enabled(slug, enabled)
            .await
            .map(Self::app_dto)
            .map_err(map_err)
    }

    async fn disconnect(&self, slug: &str) -> Result<ComposioDisconnectDto, ComposioControlError> {
        let remote_removed = self.svc.disconnect(slug).await.map_err(map_err)?;
        Ok(ComposioDisconnectDto { remote_removed })
    }

    async fn test(&self, slug: &str) -> Result<ComposioTestDto, ComposioControlError> {
        let out = self.svc.test_app(slug).await.map_err(map_err)?;
        Ok(ComposioTestDto {
            authorization_only: out.authorization_only,
            account: out.account,
            items: out.items,
        })
    }

    async fn set_bots(
        &self,
        slug: &str,
        bots: Option<Vec<String>>,
    ) -> Result<ComposioAppDto, ComposioControlError> {
        self.svc
            .set_bots(slug, bots)
            .await
            .map(Self::app_dto)
            .map_err(map_err)
    }
}
