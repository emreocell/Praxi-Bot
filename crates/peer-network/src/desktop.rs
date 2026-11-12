//! `DesktopBackend` over RPC: the serving side ([`DesktopRpcHandler`]) and the
//! calling side ([`RemoteDesktop`]).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, Quality, Region,
    Snapshot, WindowInfo,
    snapshot::{AppSpec, Pid},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::error::{PeerError, Result};
use crate::net::PeerNetwork;
use crate::proto::{
    ERR_FORBIDDEN, ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, PROTOCOL_VERSION,
    RpcError,
};
use crate::rpc::{RpcClient, RpcHandler};
use crate::store::{Device, now_secs};

/// Application error code: element not found.
pub const ERR_ELEMENT_NOT_FOUND: i64 = -32001;
/// Application error code: accessibility error.
pub const ERR_ACCESSIBILITY: i64 = -32002;
/// Application error code: screenshot error.
pub const ERR_SCREENSHOT: i64 = -32004;
/// Application error code: unsupported on the node's platform.
pub const ERR_UNSUPPORTED: i64 = -32005;
/// Application error code: internal desktop error.
pub const ERR_DESKTOP_INTERNAL: i64 = -32006;

/// Static facts about a node, returned by `node.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub os: String,
    pub arch: String,
    pub has_display: bool,
    pub has_browser: bool,
    pub version: String,
    pub protocol_version: u32,
    pub hostname: Option<String>,
}

impl NodeInfo {
    /// Collect info for this machine given the local backend's capabilities.
    #[must_use]
    pub fn detect(caps: &Capabilities) -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            has_display: caps.has_display,
            has_browser: detect_browser(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            hostname: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .ok()
                .filter(|h| !h.is_empty()),
        }
    }
}

/// Whether a Chromium-family browser or Firefox seems to be installed.
#[must_use]
pub fn detect_browser() -> bool {
    const NAMES: [&str; 6] = [
        "chrome",
        "google-chrome",
        "chromium",
        "msedge",
        "microsoft-edge",
        "firefox",
    ];
    let on_path = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            NAMES
                .iter()
                .any(|n| dir.join(n).exists() || dir.join(format!("{n}.exe")).exists())
        })
    });
    #[cfg(windows)]
    {
        if on_path {
            return true;
        }
        let roots = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"];
        let rels = [
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
            r"Mozilla Firefox\firefox.exe",
        ];
        roots.iter().filter_map(std::env::var_os).any(|root| {
            rels.iter()
                .any(|rel| std::path::Path::new(&root).join(rel).exists())
        })
    }
    #[cfg(not(windows))]
    on_path
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct ImageWire {
    data_base64: String,
    width: u32,
    height: u32,
    mime_type: String,
}

// Mirrors `Capabilities`, which has independent boolean capabilities.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize, Deserialize)]
struct CapsWire {
    platform: String,
    accessibility_tree: bool,
    screenshot: bool,
    app_launch: bool,
    has_display: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct SnapshotParams {
    #[serde(default)]
    scope: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct ActParams {
    action: Action,
}

#[derive(Serialize, Deserialize)]
struct ScreenshotParams {
    region: Region,
    quality: Quality,
}

#[derive(Serialize, Deserialize)]
struct LaunchParams {
    app: AppSpec,
}

fn desktop_to_rpc(e: &DesktopError) -> RpcError {
    match e {
        DesktopError::ElementNotFound { index, generation } => {
            RpcError::new(ERR_ELEMENT_NOT_FOUND, e.to_string())
                .with_data(json!({ "index": index, "generation": generation }))
        }
        DesktopError::Accessibility(m) => RpcError::new(ERR_ACCESSIBILITY, m.clone()),
        DesktopError::Screenshot(m) => RpcError::new(ERR_SCREENSHOT, m.clone()),
        DesktopError::Unsupported(m) => RpcError::new(ERR_UNSUPPORTED, m.clone()),
        DesktopError::Internal(m) => RpcError::new(ERR_DESKTOP_INTERNAL, m.clone()),
    }
}

fn peer_to_desktop(e: PeerError) -> DesktopError {
    match e {
        PeerError::Rpc {
            code,
            message,
            data,
        } => match code {
            ERR_ELEMENT_NOT_FOUND => {
                let field = |k: &str| data.as_ref().and_then(|d| d.get(k)).and_then(Value::as_u64);
                DesktopError::ElementNotFound {
                    index: field("index")
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0),
                    generation: field("generation").unwrap_or(0),
                }
            }
            ERR_ACCESSIBILITY => DesktopError::Accessibility(message),
            ERR_SCREENSHOT => DesktopError::Screenshot(message),
            ERR_UNSUPPORTED => DesktopError::Unsupported(message),
            _ => DesktopError::Internal(format!("remote rpc error {code}: {message}")),
        },
        other => DesktopError::Internal(format!("remote node: {other}")),
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> std::result::Result<T, RpcError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params)
        .map_err(|e| RpcError::new(ERR_INVALID_PARAMS, format!("invalid params: {e}")))
}

fn to_value<T: Serialize>(v: &T) -> std::result::Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(ERR_INTERNAL, e.to_string()))
}

// ── Serving side ──────────────────────────────────────────────────────────────

/// Serves `node.*` and `desktop.*` methods from a local [`DesktopBackend`].
pub struct DesktopRpcHandler {
    backend: Arc<dyn DesktopBackend>,
    info: NodeInfo,
}

impl DesktopRpcHandler {
    /// Wrap `backend`; `node.info` is derived from it.
    #[must_use]
    pub fn new(backend: Arc<dyn DesktopBackend>) -> Self {
        let info = NodeInfo::detect(&backend.capabilities());
        Self { backend, info }
    }

    /// Override the reported node info (tests, custom builds).
    #[must_use]
    pub fn with_info(mut self, info: NodeInfo) -> Self {
        self.info = info;
        self
    }

    /// The info this handler reports.
    #[must_use]
    pub fn info(&self) -> &NodeInfo {
        &self.info
    }
}

#[async_trait]
impl RpcHandler for DesktopRpcHandler {
    async fn handle(
        &self,
        peer: &Device,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, RpcError> {
        match method {
            "node.ping" => return Ok(json!({ "pong": true, "ts": now_secs() })),
            "node.info" => return to_value(&self.info),
            m if m.starts_with("desktop.") => {}
            other => {
                return Err(RpcError::new(
                    ERR_METHOD_NOT_FOUND,
                    format!("unknown method '{other}'"),
                ));
            }
        }
        if !peer.role.may_control() {
            return Err(RpcError::new(
                ERR_FORBIDDEN,
                format!("role '{}' may not control this machine", peer.role),
            ));
        }
        let b = &self.backend;
        match method {
            "desktop.snapshot" => {
                let p: SnapshotParams = parse(params)?;
                let s = b.snapshot(p.scope).await.map_err(|e| desktop_to_rpc(&e))?;
                to_value(&s)
            }
            "desktop.act" => {
                let p: ActParams = parse(params)?;
                let r = b.act(p.action).await.map_err(|e| desktop_to_rpc(&e))?;
                to_value(&r)
            }
            "desktop.screenshot" => {
                let p: ScreenshotParams = parse(params)?;
                let img = b
                    .screenshot(p.region, p.quality)
                    .await
                    .map_err(|e| desktop_to_rpc(&e))?;
                to_value(&ImageWire {
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&img.data),
                    width: img.width,
                    height: img.height,
                    mime_type: img.mime_type,
                })
            }
            "desktop.windows" => {
                let w = b.windows().await.map_err(|e| desktop_to_rpc(&e))?;
                to_value(&w)
            }
            "desktop.launch" => {
                let p: LaunchParams = parse(params)?;
                let pid = b.launch(p.app).await.map_err(|e| desktop_to_rpc(&e))?;
                Ok(json!({ "pid": pid }))
            }
            "desktop.capabilities" => {
                let c = b.capabilities();
                to_value(&CapsWire {
                    platform: c.platform,
                    accessibility_tree: c.accessibility_tree,
                    screenshot: c.screenshot,
                    app_launch: c.app_launch,
                    has_display: c.has_display,
                })
            }
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("unknown method '{other}'"),
            )),
        }
    }
}

// ── Calling side ──────────────────────────────────────────────────────────────

/// A paired remote machine used like a local [`DesktopBackend`].
pub struct RemoteDesktop {
    client: RpcClient,
    info: NodeInfo,
    caps: Capabilities,
}

impl RemoteDesktop {
    /// Connect to a paired node, verify protocol compatibility and fetch its
    /// info and capabilities.
    ///
    /// # Errors
    ///
    /// Fails if the peer is unknown or unreachable, speaks another protocol
    /// version ([`PeerError::VersionMismatch`]), or refuses the link.
    pub async fn connect(net: &PeerNetwork, peer: &iroh::PublicKey) -> Result<Self> {
        let client = net.rpc_client(peer)?;
        let info: NodeInfo = decode(client.call("node.info", Value::Null).await?)?;
        if info.protocol_version != PROTOCOL_VERSION {
            return Err(PeerError::VersionMismatch {
                local: PROTOCOL_VERSION,
                remote: info.protocol_version,
            });
        }
        let c: CapsWire = decode(client.call("desktop.capabilities", Value::Null).await?)?;
        let caps = Capabilities {
            platform: c.platform,
            accessibility_tree: c.accessibility_tree,
            screenshot: c.screenshot,
            app_launch: c.app_launch,
            has_display: c.has_display,
        };
        Ok(Self { client, info, caps })
    }

    /// What `node.info` reported at connect time.
    #[must_use]
    pub fn node_info(&self) -> &NodeInfo {
        &self.info
    }

    /// The underlying RPC client (for `node.ping` or custom methods).
    #[must_use]
    pub fn client(&self) -> &RpcClient {
        &self.client
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<T, DesktopError> {
        let v = self
            .client
            .call(method, params)
            .await
            .map_err(peer_to_desktop)?;
        decode(v).map_err(peer_to_desktop)
    }
}

fn decode<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| PeerError::Protocol(format!("bad result: {e}")))
}

fn enc<T: Serialize>(v: &T) -> std::result::Result<Value, DesktopError> {
    serde_json::to_value(v).map_err(|e| DesktopError::Internal(e.to_string()))
}

#[async_trait]
impl DesktopBackend for RemoteDesktop {
    async fn snapshot(&self, scope: Option<u64>) -> std::result::Result<Snapshot, DesktopError> {
        self.call("desktop.snapshot", enc(&SnapshotParams { scope })?)
            .await
    }

    async fn act(&self, action: Action) -> std::result::Result<ActionResult, DesktopError> {
        self.call("desktop.act", enc(&ActParams { action })?).await
    }

    async fn screenshot(
        &self,
        region: Region,
        quality: Quality,
    ) -> std::result::Result<Image, DesktopError> {
        let w: ImageWire = self
            .call(
                "desktop.screenshot",
                enc(&ScreenshotParams { region, quality })?,
            )
            .await?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(w.data_base64)
            .map_err(|e| DesktopError::Screenshot(format!("bad image data from node: {e}")))?;
        Ok(Image {
            data,
            width: w.width,
            height: w.height,
            mime_type: w.mime_type,
        })
    }

    async fn windows(&self) -> std::result::Result<Vec<WindowInfo>, DesktopError> {
        self.call("desktop.windows", Value::Null).await
    }

    async fn launch(&self, app: AppSpec) -> std::result::Result<Pid, DesktopError> {
        let v: Value = self
            .call("desktop.launch", enc(&LaunchParams { app })?)
            .await?;
        v.get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| Pid::try_from(p).ok())
            .ok_or_else(|| DesktopError::Internal("launch result lacks pid".into()))
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }
}
