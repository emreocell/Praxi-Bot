//! Per-bot computer target configuration and run-start resolution (ADR-0027 §7).
//!
//! A bot's profile declares where it runs UI automation (`[runtime].computer`),
//! defaulting to `auto`. At the start of each top-level run, [`resolve`] maps
//! that preference plus the current availability supplied by [`ComputerProvider`]
//! to a fixed [`Resolution`] for the lifetime of the run.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use desktop_api::DesktopBackend;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Durable pause reason stored on a run waiting for a paired PC node.
pub const WAIT_FOR_PC_REASON: &str = "waiting_for_pc";

/// Where a bot may execute computer actions (`[runtime].computer`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ComputerTarget {
    /// Automatic selection per ADR-0027 §7 (PC if online / local PC install,
    /// otherwise host Xvfb desktop, otherwise headless browser, otherwise off).
    #[default]
    Auto,
    /// Paired PC node (or local desktop on a non-host PC install).
    /// Pauses waiting (`wait_for_pc = true`) when the PC is offline on a host.
    Pc,
    /// Host machine's local desktop (e.g. Linux Xvfb); on a non-host install,
    /// same as local desktop.
    Host,
    /// Headless browser only; no desktop backend.
    Browser,
    /// Neither desktop nor browser tools.
    Off,
    /// Specific paired node identifier (legacy ADR-0011 syntax, e.g. `node:<id>`
    /// or 64-character hex peer public key).
    Node(String),
}

impl ComputerTarget {
    /// Canonical string representation stored in configs and serialized DTOs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Auto => "auto",
            Self::Pc => "pc",
            Self::Host => "host",
            Self::Browser => "browser",
            Self::Off => "off",
            Self::Node(id) => id.as_str(),
        }
    }

    /// Raw peer identifier when targeting an explicit [`Self::Node`], stripping
    /// optional `node:` / `peer:` prefixes.
    #[must_use]
    pub fn node_id(&self) -> Option<&str> {
        match self {
            Self::Node(id) => Some(
                id.strip_prefix("node:")
                    .or_else(|| id.strip_prefix("peer:"))
                    .unwrap_or(id),
            ),
            _ => None,
        }
    }
}

fn is_valid_node_id(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("node:").or_else(|| s.strip_prefix("peer:")) {
        return !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    }
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl FromStr for ComputerTarget {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "auto" => Ok(Self::Auto),
            "pc" | "local" => Ok(Self::Pc),
            "host" => Ok(Self::Host),
            "browser" => Ok(Self::Browser),
            "off" => Ok(Self::Off),
            other if is_valid_node_id(other) => Ok(Self::Node(other.to_owned())),
            other => Err(format!(
                "unknown computer target {other:?}; expected one of auto, pc, host, browser, off, or node:<id>"
            )),
        }
    }
}

impl fmt::Display for ComputerTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ComputerTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ComputerTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

/// `[runtime]` section of a bot profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Configured computer target (default `auto`).
    pub computer: ComputerTarget,
}

/// Resolved effective target label stored on each run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerLabel {
    Pc,
    Host,
    Browser,
    Off,
}

impl ComputerLabel {
    /// Canonical lowercase label string (`pc`, `host`, `browser`, `off`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pc => "pc",
            Self::Host => "host",
            Self::Browser => "browser",
            Self::Off => "off",
        }
    }

    /// Parse a stored label string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pc" => Some(Self::Pc),
            "host" => Some(Self::Host),
            "browser" => Some(Self::Browser),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

impl fmt::Display for ComputerLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable identity of a resolved desktop. `local` denotes this daemon's desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerNode {
    pub id: String,
    pub name: String,
}

/// A backend and its identity, sampled atomically by the provider.
pub struct ComputerDesktop {
    pub node: ComputerNode,
    pub backend: Arc<dyn DesktopBackend>,
}

/// Runtime availability queried when resolving a bot's [`ComputerTarget`].
pub trait ComputerProvider: Send + Sync {
    /// Local desktop backend (host Xvfb on Linux host, or PC desktop on non-host install).
    fn local(&self) -> Option<Arc<dyn DesktopBackend>>;

    /// Display identity of this daemon's local computer.
    fn local_node(&self) -> ComputerNode {
        ComputerNode {
            id: "local".into(),
            name: "Local computer".into(),
        }
    }

    /// Connected paired PC desktop backend (from the host daemon).
    fn pc(&self) -> Option<Arc<dyn DesktopBackend>>;

    /// Atomically select a remote PC and its stable key. No anonymous remote is safe.
    fn pc_target(&self) -> Option<ComputerDesktop> {
        None
    }

    /// Identity of an explicit node, matched by key only.
    fn node_target(&self, _id: &str) -> Option<ComputerDesktop> {
        None
    }

    /// Whether remote PC networking can become available.
    fn pc_network_enabled(&self) -> bool {
        true
    }

    /// Whether this daemon runs in always-on host mode (`--host`).
    fn is_host(&self) -> bool;

    /// Whether a local Chromium-family browser can be launched.
    fn browser_available(&self) -> bool;

    /// Optional lookup for a specific paired node identifier (`ComputerTarget::Node`).
    fn node(&self, _id: &str) -> Option<Arc<dyn DesktopBackend>> {
        None
    }

    /// Whether the local environment has an active graphical display (e.g. Xvfb
    /// `$DISPLAY` on the host or a native desktop session).
    fn has_display(&self) -> bool {
        self.local().is_some()
    }

    /// Optional async warm-up before [`resolve`] runs at turn start (e.g.
    /// establishing or reconnecting an on-demand remote desktop RPC client).
    fn prepare(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

/// Resolved computer capabilities fixed at the start of a run.
#[derive(Clone)]
pub struct Resolution {
    pub node: Option<ComputerNode>,
    pub unavailable_reason: Option<String>,
    /// Desktop automation backend for this run (`None` hides desktop tools).
    pub desktop: Option<Arc<dyn DesktopBackend>>,
    /// Whether browser tools are enabled for this run.
    pub browser: bool,
    /// Human-visible resolution label (`pc`, `host`, `browser`, `off`).
    pub label: ComputerLabel,
    /// True when the bot requires a PC desktop that is currently offline.
    pub wait_for_pc: bool,
}

impl Resolution {
    /// Whether browser launches for this run should use `--headless=new`.
    ///
    /// Host resolutions with an active display launch headed (e.g. on Xvfb);
    /// browser-only and displayless resolutions run headless.
    #[must_use]
    pub fn browser_headless(&self, has_display: bool) -> bool {
        !(self.label == ComputerLabel::Host && has_display)
    }
}

impl fmt::Debug for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolution")
            .field("node", &self.node)
            .field("unavailable_reason", &self.unavailable_reason)
            .field("has_desktop", &self.desktop.is_some())
            .field("browser", &self.browser)
            .field("label", &self.label)
            .field("wait_for_pc", &self.wait_for_pc)
            .finish()
    }
}

fn browser_or_off(browser: bool) -> Resolution {
    if browser {
        Resolution {
            node: None,
            unavailable_reason: None,
            desktop: None,
            browser: true,
            label: ComputerLabel::Browser,
            wait_for_pc: false,
        }
    } else {
        Resolution {
            node: None,
            unavailable_reason: None,
            desktop: None,
            browser: false,
            label: ComputerLabel::Off,
            wait_for_pc: false,
        }
    }
}

fn pc_desktop(provider: &dyn ComputerProvider) -> Option<ComputerDesktop> {
    provider.pc_target().or_else(|| {
        if provider.is_host() {
            return None;
        }
        provider.local().map(|backend| ComputerDesktop {
            node: provider.local_node(),
            backend,
        })
    })
}

fn pc_resolution(desktop: Option<ComputerDesktop>, provider: &dyn ComputerProvider) -> Resolution {
    let unavailable_reason = (desktop.is_none() && !provider.pc_network_enabled())
        .then(|| "PC desktop unavailable: peer networking is disabled".into());
    let local = desktop.as_ref().is_some_and(|d| d.node.id == "local");
    Resolution {
        wait_for_pc: desktop.is_none() && unavailable_reason.is_none(),
        browser: local && provider.browser_available(),
        label: ComputerLabel::Pc,
        node: desktop.as_ref().map(|d| d.node.clone()),
        desktop: desktop.map(|d| d.backend),
        unavailable_reason,
    }
}

fn resolve_auto(provider: &dyn ComputerProvider, browser: bool) -> Resolution {
    if let Some(d) = pc_desktop(provider) {
        return pc_resolution(Some(d), provider);
    }
    if provider.is_host()
        && let Some(d) = provider.local()
    {
        return Resolution {
            node: Some(provider.local_node()),
            unavailable_reason: None,
            desktop: Some(d),
            browser,
            label: ComputerLabel::Host,
            wait_for_pc: false,
        };
    }
    browser_or_off(browser)
}

/// Resolve a bot's [`ComputerTarget`] against current [`ComputerProvider`] state
/// according to ADR-0027 §7.
#[must_use]
pub fn resolve(target: &ComputerTarget, provider: &dyn ComputerProvider) -> Resolution {
    let browser = provider.browser_available();
    match target {
        ComputerTarget::Off => browser_or_off(false),
        ComputerTarget::Browser => browser_or_off(browser),
        ComputerTarget::Host => match provider.local() {
            Some(d) => Resolution {
                node: Some(provider.local_node()),
                unavailable_reason: None,
                desktop: Some(d),
                browser,
                label: ComputerLabel::Host,
                wait_for_pc: false,
            },
            None => Resolution {
                node: None,
                unavailable_reason: Some(
                    "Host desktop unavailable: no local desktop/display".into(),
                ),
                desktop: None,
                browser: false,
                label: ComputerLabel::Host,
                wait_for_pc: false,
            },
        },
        ComputerTarget::Pc => pc_resolution(pc_desktop(provider), provider),
        ComputerTarget::Node(id) => {
            let norm = target.node_id().unwrap_or(id.as_str());
            let mut res = pc_resolution(provider.node_target(norm), provider);
            if res.node.is_none() {
                res.node = Some(ComputerNode {
                    id: norm.into(),
                    name: norm.into(),
                });
            }
            res
        }
        ComputerTarget::Auto => resolve_auto(provider, browser),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use desktop_api::Capabilities;
    use desktop_api::action::{Action, ActionResult};
    use desktop_api::error::DesktopError;
    use desktop_api::snapshot::{AppSpec, Image, Pid, Quality, Region, Snapshot, WindowInfo};

    struct DummyDesktop(&'static str);

    #[async_trait]
    impl DesktopBackend for DummyDesktop {
        async fn snapshot(&self, _scope: Option<u64>) -> Result<Snapshot, DesktopError> {
            Err(DesktopError::Unsupported("dummy".into()))
        }
        async fn act(&self, _action: Action) -> Result<ActionResult, DesktopError> {
            Err(DesktopError::Unsupported("dummy".into()))
        }
        async fn screenshot(
            &self,
            _region: Region,
            _quality: Quality,
        ) -> Result<Image, DesktopError> {
            Err(DesktopError::Unsupported("dummy".into()))
        }
        async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
            Ok(Vec::new())
        }
        async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
            Err(DesktopError::Unsupported("dummy".into()))
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                platform: self.0.into(),
                accessibility_tree: true,
                screenshot: true,
                app_launch: true,
                has_display: true,
            }
        }
    }

    struct MockProvider {
        local: Option<Arc<dyn DesktopBackend>>,
        pc: Option<Arc<dyn DesktopBackend>>,
        node: Option<Arc<dyn DesktopBackend>>,
        is_host: bool,
        browser: bool,
    }

    impl ComputerProvider for MockProvider {
        fn local(&self) -> Option<Arc<dyn DesktopBackend>> {
            self.local.clone()
        }
        fn pc(&self) -> Option<Arc<dyn DesktopBackend>> {
            self.pc.clone()
        }
        fn pc_target(&self) -> Option<ComputerDesktop> {
            self.pc.clone().map(|backend| ComputerDesktop {
                node: ComputerNode {
                    id: "pc-key".into(),
                    name: "PC".into(),
                },
                backend,
            })
        }
        fn node_target(&self, id: &str) -> Option<ComputerDesktop> {
            self.node(id).map(|backend| ComputerDesktop {
                node: ComputerNode {
                    id: id.into(),
                    name: "Node".into(),
                },
                backend,
            })
        }
        fn is_host(&self) -> bool {
            self.is_host
        }
        fn browser_available(&self) -> bool {
            self.browser
        }
        fn node(&self, id: &str) -> Option<Arc<dyn DesktopBackend>> {
            (id == "workstation-1").then(|| self.node.clone()).flatten()
        }
    }

    fn dummy(tag: &'static str) -> Arc<dyn DesktopBackend> {
        Arc::new(DummyDesktop(tag))
    }

    #[test]
    fn parse_defaults_five_values_aliases_and_rejects_unknown() {
        assert_eq!(RuntimeConfig::default().computer, ComputerTarget::Auto);

        for (raw, expected) in [
            ("auto", ComputerTarget::Auto),
            ("pc", ComputerTarget::Pc),
            ("local", ComputerTarget::Pc),
            ("host", ComputerTarget::Host),
            ("browser", ComputerTarget::Browser),
            ("off", ComputerTarget::Off),
            (
                "node:workstation-1",
                ComputerTarget::Node("node:workstation-1".into()),
            ),
            ("peer:abc_123", ComputerTarget::Node("peer:abc_123".into())),
        ] {
            let parsed: ComputerTarget = raw.parse().unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(parsed, expected);
        }

        let hex64 = "0123456789abcdef".repeat(4);
        assert_eq!(
            hex64.parse::<ComputerTarget>().unwrap(),
            ComputerTarget::Node(hex64)
        );

        for bad in [
            "",
            "cloud",
            "vps",
            "AUTO",
            "desktop",
            "node:",
            "node:bad id",
            "shorthex",
        ] {
            assert!(
                bad.parse::<ComputerTarget>().is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn resolve_matrix_on_host() {
        // Host with PC online, Xvfb local, and browser available.
        let all_online = MockProvider {
            local: Some(dummy("host")),
            pc: Some(dummy("pc")),
            node: Some(dummy("node")),
            is_host: true,
            browser: true,
        };
        let r = resolve(&ComputerTarget::Auto, &all_online);
        assert_eq!(r.label, ComputerLabel::Pc);
        assert_eq!(r.desktop.as_ref().unwrap().capabilities().platform, "pc");
        assert!(!r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Host, &all_online);
        assert_eq!(r.label, ComputerLabel::Host);
        assert_eq!(r.desktop.as_ref().unwrap().capabilities().platform, "host");
        assert!(r.browser);
        assert!(!r.wait_for_pc);
        assert!(!r.browser_headless(true));
        assert!(r.browser_headless(false));

        let r = resolve(&ComputerTarget::Browser, &all_online);
        assert_eq!(r.label, ComputerLabel::Browser);
        assert!(r.desktop.is_none());
        assert!(r.browser);
        assert!(!r.wait_for_pc);
        assert!(r.browser_headless(true));

        let r = resolve(&ComputerTarget::Off, &all_online);
        assert_eq!(r.label, ComputerLabel::Off);
        assert!(r.desktop.is_none());
        assert!(!r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(
            &ComputerTarget::Node("node:workstation-1".into()),
            &all_online,
        );
        assert_eq!(r.label, ComputerLabel::Pc);
        assert_eq!(r.desktop.as_ref().unwrap().capabilities().platform, "node");
        assert!(!r.wait_for_pc);

        // Host with PC offline, Xvfb local + browser present.
        let host_pc_offline = MockProvider {
            local: Some(dummy("host")),
            pc: None,
            node: None,
            is_host: true,
            browser: true,
        };
        let r = resolve(&ComputerTarget::Auto, &host_pc_offline);
        assert_eq!(r.label, ComputerLabel::Host);
        assert_eq!(r.desktop.as_ref().unwrap().capabilities().platform, "host");
        assert!(r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Pc, &host_pc_offline);
        assert_eq!(r.label, ComputerLabel::Pc);
        assert!(r.desktop.is_none());
        assert!(!r.browser);
        assert!(r.wait_for_pc);

        let r = resolve(
            &ComputerTarget::Node("node:workstation-1".into()),
            &host_pc_offline,
        );
        assert_eq!(r.label, ComputerLabel::Pc);
        assert!(r.desktop.is_none());
        assert!(r.wait_for_pc);

        // Host with neither PC nor Xvfb, browser present -> Auto falls back to Browser.
        let host_browser_only = MockProvider {
            local: None,
            pc: None,
            node: None,
            is_host: true,
            browser: true,
        };
        let r = resolve(&ComputerTarget::Auto, &host_browser_only);
        assert_eq!(r.label, ComputerLabel::Browser);
        assert!(r.desktop.is_none());
        assert!(r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Host, &host_browser_only);
        assert_eq!(r.label, ComputerLabel::Host);
        assert!(r.desktop.is_none());
        assert!(!r.browser);
        assert!(r.unavailable_reason.unwrap().contains("no local desktop"));
    }

    #[test]
    fn resolve_matrix_on_non_host_and_fallbacks() {
        // Host with nothing available -> Auto resolves to Off.
        let nothing = MockProvider {
            local: None,
            pc: None,
            node: None,
            is_host: true,
            browser: false,
        };
        let r = resolve(&ComputerTarget::Auto, &nothing);
        assert_eq!(r.label, ComputerLabel::Off);
        assert!(r.desktop.is_none());
        assert!(!r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Browser, &nothing);
        assert_eq!(r.label, ComputerLabel::Off);
        assert!(r.desktop.is_none());
        assert!(!r.browser);

        // Non-host install (daemon runs directly on the user's PC).
        let local_pc = MockProvider {
            local: Some(dummy("local-pc")),
            pc: None,
            node: None,
            is_host: false,
            browser: true,
        };
        let r = resolve(&ComputerTarget::Auto, &local_pc);
        assert_eq!(r.label, ComputerLabel::Pc);
        assert_eq!(
            r.desktop.as_ref().unwrap().capabilities().platform,
            "local-pc"
        );
        assert!(r.browser);
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Pc, &local_pc);
        assert_eq!(r.label, ComputerLabel::Pc);
        assert_eq!(
            r.desktop.as_ref().unwrap().capabilities().platform,
            "local-pc"
        );
        assert!(!r.wait_for_pc);

        let r = resolve(&ComputerTarget::Host, &local_pc);
        assert_eq!(r.label, ComputerLabel::Host);
        assert_eq!(
            r.desktop.as_ref().unwrap().capabilities().platform,
            "local-pc"
        );
        assert!(!r.wait_for_pc);
    }
}
