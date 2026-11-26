//! The server as the tool target of its paired ADMIN desktop (gap 2 of card 272).
//!
//! The bot's brain (model providers, keys, subscription CLIs, approvals) stays
//! on the owner's computer; this host only executes the browser calls that one
//! desktop sends, with the same tool code it runs locally, and audits every call.
//!
//! Authority, all checked here before anything runs:
//! 1. Only the paired device with role `desktop` AND the `admin` flag, read from
//!    the pairing file again for every request (revoking, re-pairing or removing
//!    the admin flag stops the next call). A phone, a node, a host, another
//!    desktop without the flag and an unknown key are refused and the refusal is
//!    audited.
//! 2. Only the browser family is served, and only what the PC side serves too:
//!    web pages (`http`/`https`), no page scripts, no stored secrets, no profile
//!    choice, no typing into password fields. No shell, no files, no desktop.
//! 3. The served browser never reaches this server's own network: not loopback
//!    (the daemon API, admin UIs), not link-local or cloud metadata, and not a
//!    private range either (nobody at this host can allow one).
//! 4. Approvals are not asked here: the owner is at the brain's computer and
//!    answers there. Every call, allowed or refused, is written to the audit
//!    log; a call whose audit row cannot be written does not run.
//!
//! Nothing new listens: requests arrive over the existing paired iroh link.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use builtin_tools::BrowserDriver;
use builtin_tools::pc::{PcInfo, RPC_BROWSER, RPC_INFO};
use parking_lot::Mutex;
use peer_network::proto::{ERR_FORBIDDEN, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, RpcError};
use peer_network::{Device, Role, RpcHandler};
use serde_json::Value;

use crate::pc_target::{
    AuditFn, BrowserFactory, caller_of, complete_text, is_navigation_key, run_browser_op, web_url,
};

/// At most this many bots' browsers are open on the server at once; the oldest
/// is closed.
const MAX_BROWSERS: usize = 8;

/// Longest address `open` accepts.
const MAX_URL_CHARS: usize = 2000;

/// Everything the handler needs from the host daemon.
pub struct ServerTargetDeps {
    /// The host's data directory (the pairing file lives under `peer/`).
    pub data: PathBuf,
    pub audit: AuditFn,
    /// Mask owner secrets in anything that leaves this host.
    pub redact: Arc<dyn Fn(&str) -> String + Send + Sync>,
    /// Makes this host's managed browser, one per bot (its own process and
    /// profile: tabs, page state and cookies are never shared between bots).
    /// `None`: the browser is not served.
    pub browser: Option<BrowserFactory>,
}

/// Serves `pc.info` and `pc.browser` to the paired admin desktop.
pub struct ServerTargetHandler {
    deps: ServerTargetDeps,
    /// One browser per bot, oldest first.
    browsers: Mutex<Vec<(String, Arc<dyn BrowserDriver>)>>,
}

impl ServerTargetHandler {
    #[must_use]
    pub fn new(deps: ServerTargetDeps) -> Self {
        Self {
            deps,
            browsers: Mutex::new(Vec::new()),
        }
    }

    /// The served browser of one bot: made on first use, with the network
    /// policy that keeps it out of this server's own network.
    fn browser_for(&self, bot: &str) -> Option<Arc<dyn BrowserDriver>> {
        let make = self.deps.browser.as_ref()?;
        let mut open = self.browsers.lock();
        if let Some((_, b)) = open.iter().find(|(k, _)| k == bot) {
            return Some(Arc::clone(b));
        }
        let driver = make();
        driver.set_served_network(browser_cdp::ServedNetwork {
            check: browser_cdp::DestinationCheck::new(|url| async move {
                match browser_cdp::classify_url(&url).await {
                    browser_cdp::Verdict::Allow => Ok(()),
                    browser_cdp::Verdict::Block(why) => Err(format!("blocked: {why}")),
                    browser_cdp::Verdict::Private(host) => Err(format!(
                        "blocked: {host} is inside this server's own network"
                    )),
                }
            }),
            // The proxy resolved a private-range host: nobody here may allow it.
            private_host: browser_cdp::DestinationCheck::new(|host: String| async move {
                Err(format!(
                    "blocked: {host} is inside this server's own network"
                ))
            }),
        });
        open.push((bot.to_owned(), Arc::clone(&driver)));
        while open.len() > MAX_BROWSERS {
            let (_, old) = open.remove(0);
            tokio::spawn(async move { old.close_all().await });
        }
        Some(driver)
    }

    fn refuse(&self, peer: &Device, method: &str, why: &str) -> RpcError {
        let _ = (self.deps.audit)(
            "server_target.refused",
            serde_json::json!({"peer": peer.id, "role": peer.role.to_string(), "method": method, "why": why}),
        );
        RpcError::new(ERR_FORBIDDEN, why)
    }

    /// The pairing as it is on disk now: the same device, the same pairing,
    /// role `desktop`, administration granted by this host's owner.
    fn authorize(&self, peer: &Device, method: &str) -> Result<(), RpcError> {
        let Some(now) = crate::pc_target::paired_device(&self.deps.data, &peer.id) else {
            return Err(self.refuse(peer, method, "not paired with this server"));
        };
        if now.pairing_generation != peer.pairing_generation {
            return Err(self.refuse(peer, method, "paired again since this request started"));
        }
        if now.role != Role::Desktop || !now.admin {
            return Err(self.refuse(
                peer,
                method,
                "only the paired administrator desktop may use this server as a computer",
            ));
        }
        Ok(())
    }

    fn info(&self) -> Result<Value, RpcError> {
        let info = PcInfo {
            os: std::env::consts::OS.to_owned(),
            shell: if cfg!(windows) { "PowerShell" } else { "sh" }.to_owned(),
            home: None,
            hostname: None,
            screen: false,
            browser: self.deps.browser.is_some(),
            commands: false,
            files: false,
        };
        serde_json::to_value(info).map_err(|e| RpcError::new(-32603, e.to_string()))
    }

    /// What a page must not be able to make the bot type: a password field,
    /// invisible text, or a key chord into a secret field.
    async fn guard_input(
        &self,
        peer: &Device,
        driver: &Arc<dyn BrowserDriver>,
        op: &str,
        params: &Value,
    ) -> Result<(), RpcError> {
        let method = RPC_BROWSER;
        let arg = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        if op == "fill" {
            complete_text(&arg("text")).map_err(|why| self.refuse(peer, method, &why))?;
            // A password / one-time-code / card field is never filled by a bot.
            // Fails closed: when the browser cannot say what the field is, the
            // fill is refused.
            match driver.field_target(&arg("ref")).await {
                Ok(f) if !f.sensitive => {}
                Ok(_) => {
                    return Err(self.refuse(peer, method, "the element is a password field"));
                }
                Err(_) => {
                    return Err(self.refuse(
                        peer,
                        method,
                        "cannot tell whether the element is a password field",
                    ));
                }
            }
        } else {
            let key = arg("key");
            complete_text(&key).map_err(|why| self.refuse(peer, method, &why))?;
            if !is_navigation_key(&key)
                && driver.focused_target().await.map_or(true, |f| f.sensitive)
            {
                return Err(self.refuse(
                    peer,
                    method,
                    "the focused element is, or may be, a password field; only Tab, Enter, Escape and arrow keys are allowed there",
                ));
            }
        }
        Ok(())
    }

    async fn browser(&self, peer: &Device, params: Value) -> Result<Value, RpcError> {
        let method = RPC_BROWSER;
        let caller = caller_of(&params)
            .map_err(|why| self.refuse(peer, method, &why))?
            .ok_or_else(|| {
                self.refuse(
                    peer,
                    method,
                    "the request did not say which bot is asking, so it was refused",
                )
            })?;
        let Some(driver) = self.browser_for(&caller.id) else {
            return Err(self.refuse(peer, method, "this server has no browser to use"));
        };
        let op = params
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(ERR_INVALID_PARAMS, "missing op"))?
            .to_owned();
        let arg = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        if !matches!(
            op.as_str(),
            "open"
                | "snapshot"
                | "click"
                | "fill"
                | "select"
                | "press"
                | "back"
                | "scroll"
                | "tabs"
                | "wait"
                | "screenshot"
                | "current_url"
                | "current_tab_id"
        ) {
            return Err(self.refuse(peer, method, "that browser operation is not served"));
        }
        let mut detail = serde_json::json!({"peer": peer.id, "bot": caller.id, "op": op});
        match op.as_str() {
            "open" => {
                let url = arg("url");
                if web_url(&url).is_none() || url.chars().count() > MAX_URL_CHARS {
                    return Err(self.refuse(
                        peer,
                        method,
                        "only http and https pages can be opened",
                    ));
                }
                if browser_cdp::classify_url(&url).await != browser_cdp::Verdict::Allow {
                    return Err(self.refuse(
                        peer,
                        method,
                        "that address is inside this server's own network and is not reachable",
                    ));
                }
                detail["url"] = Value::String((self.deps.redact)(&url));
            }
            "fill" | "press" => self.guard_input(peer, &driver, &op, &params).await?,
            _ => {}
        }
        (self.deps.audit)("server_target.browser", detail).map_err(|e| {
            RpcError::new(
                ERR_FORBIDDEN,
                format!("not done: the audit log of this server could not be written ({e})"),
            )
        })?;
        run_browser_op(&driver, &op, &params, &self.deps.redact).await
    }
}

#[async_trait]
impl RpcHandler for ServerTargetHandler {
    async fn handle(&self, peer: &Device, method: &str, params: Value) -> Result<Value, RpcError> {
        if method == "node.ping" {
            return Ok(serde_json::json!({ "pong": true }));
        }
        self.authorize(peer, method)?;
        match method {
            RPC_INFO => self.info(),
            RPC_BROWSER => self.browser(peer, params).await,
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("unknown method '{other}'"),
            )),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Default)]
    pub(crate) struct Browser {
        pub(crate) opened: Mutex<Vec<String>>,
        net: Mutex<Option<browser_cdp::ServedNetwork>>,
        sensitive: bool,
    }

    #[async_trait]
    impl BrowserDriver for Browser {
        fn set_served_network(&self, net: browser_cdp::ServedNetwork) {
            *self.net.lock() = Some(net);
        }
        async fn open(&self, url: &str, _: Option<&str>) -> Result<String, String> {
            self.opened.lock().push(url.to_owned());
            Ok(format!("opened {url} token hunter2"))
        }
        async fn snapshot(&self, _: bool) -> Result<String, String> {
            Ok("page".into())
        }
        async fn click(&self, _: &str) -> Result<String, String> {
            Ok("clicked".into())
        }
        async fn fill(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("filled".into())
        }
        async fn field_target(&self, _: &str) -> Result<browser_cdp::FieldTarget, String> {
            Ok(browser_cdp::FieldTarget {
                name: "field".into(),
                role: "textbox".into(),
                frame_origin: None,
                top_origin: None,
                sensitive: self.sensitive,
            })
        }
        async fn select(&self, _: &str, _: &str) -> Result<String, String> {
            Ok("selected".into())
        }
        async fn press(&self, _: &str) -> Result<String, String> {
            Ok("pressed".into())
        }
        async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
            Ok("scrolled".into())
        }
        async fn wait(
            &self,
            _: builtin_tools::Wait,
            _: Option<std::time::Duration>,
        ) -> Result<String, String> {
            Ok("waited".into())
        }
        async fn back(&self) -> Result<String, String> {
            Ok("back".into())
        }
        async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
            Ok("tabs".into())
        }
        async fn screenshot(
            &self,
            _: bool,
            _: Option<&str>,
        ) -> Result<(Vec<u8>, &'static str), String> {
            Ok((vec![1], "png"))
        }
        async fn eval(&self, _: &str) -> Result<Value, String> {
            Err("no".into())
        }
    }

    fn device(role: Role, admin: bool) -> Device {
        Device {
            id: "desk1".into(),
            name: "My desktop".into(),
            role,
            capabilities: vec![],
            addr: None,
            paired_at: 1,
            pairing_generation: "g1".into(),
            admin,
        }
    }

    fn write_pairing(dir: &std::path::Path, role: &str, admin: bool) {
        std::fs::create_dir_all(dir.join("peer")).unwrap();
        std::fs::write(
            dir.join("peer").join("devices.json"),
            serde_json::json!({"version":1,"devices":[{
                "id":"desk1","name":"My desktop","role":role,"capabilities":[],
                "addr":null,"paired_at":1,"pairing_generation":"g1","admin":admin
            }]})
            .to_string(),
        )
        .unwrap();
    }

    struct Rig {
        h: ServerTargetHandler,
        browser: Arc<Browser>,
        audit: Arc<Mutex<Vec<String>>>,
        dir: tempfile::TempDir,
    }

    fn rig(role: &str, admin: bool, sensitive: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        write_pairing(dir.path(), role, admin);
        let browser = Arc::new(Browser {
            sensitive,
            ..Browser::default()
        });
        let audit = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&audit);
        let b = Arc::clone(&browser);
        let h = ServerTargetHandler::new(ServerTargetDeps {
            data: dir.path().to_path_buf(),
            audit: Arc::new(move |action, _| {
                sink.lock().push(action.to_owned());
                Ok(())
            }),
            redact: Arc::new(|t| t.replace("hunter2", "••••")),
            browser: Some(Arc::new(move || Arc::clone(&b) as Arc<dyn BrowserDriver>)),
        });
        Rig {
            h,
            browser,
            audit,
            dir,
        }
    }

    fn call(op: &str, extra: &Value) -> Value {
        let mut v = serde_json::json!({"op": op, "bot": {"id": "bot-1", "name": "Mausi"}});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    fn open(url: &str) -> Value {
        call("open", &serde_json::json!({"url": url}))
    }

    /// The server's browser is served to the paired administrator desktop and to
    /// nobody else: not a phone, a node, another host, a desktop without the
    /// administrator flag, and not a device the pairing file does not list.
    #[tokio::test]
    async fn only_the_paired_admin_desktop_may_use_the_servers_browser() {
        let r = rig("desktop", true, false);
        let ok =
            r.h.handle(
                &device(Role::Desktop, true),
                RPC_BROWSER,
                open("https://example.com/"),
            )
            .await
            .unwrap();
        assert!(ok.to_string().contains("opened https://example.com/"));
        assert!(!ok.to_string().contains("hunter2"), "output is masked");
        assert_eq!(r.browser.opened.lock().len(), 1);
        // The role the peer connection claims is not what counts: the pairing file is.
        for role in ["phone", "node", "host"] {
            let r = rig(role, true, false);
            let res =
                r.h.handle(
                    &device(Role::Desktop, true),
                    RPC_BROWSER,
                    open("https://example.com/"),
                )
                .await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{role}");
            assert!(r.browser.opened.lock().is_empty());
        }
        // A desktop the owner did not make an administrator.
        let r = rig("desktop", false, false);
        let res =
            r.h.handle(
                &device(Role::Desktop, true),
                RPC_BROWSER,
                open("https://example.com/"),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(r.browser.opened.lock().is_empty());
        assert!(r.audit.lock().contains(&"server_target.refused".to_owned()));
        // Unpaired between two requests.
        let r = rig("desktop", true, false);
        std::fs::remove_file(r.dir.path().join("peer").join("devices.json")).unwrap();
        let res =
            r.h.handle(
                &device(Role::Desktop, true),
                RPC_BROWSER,
                open("https://example.com/"),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(r.browser.opened.lock().is_empty());
    }

    /// The served browser never reaches the server's own network, directly or
    /// through a redirect or subresource, and nobody there can allow a private host.
    #[tokio::test]
    async fn the_served_browser_cannot_reach_the_servers_internal_services() {
        let r = rig("desktop", true, false);
        let me = device(Role::Desktop, true);
        for url in [
            "http://127.0.0.1:8123/v1/status",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
        ] {
            let res = r.h.handle(&me, RPC_BROWSER, open(url)).await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{url}");
        }
        assert!(r.browser.opened.lock().is_empty());
        // A first allowed call builds the browser and installs the policy on it.
        r.h.handle(&me, RPC_BROWSER, open("https://93.184.216.34/"))
            .await
            .unwrap();
        let net = r.browser.net.lock().take().expect("a policy is installed");
        for url in [
            "http://127.0.0.1:8123/",
            "http://[::1]/",
            "http://169.254.169.254/",
            "http://192.168.0.10/",
        ] {
            assert!(net.check.check(url.into()).await.is_err(), "{url}");
        }
        assert!(
            net.private_host
                .check("intranet.local".into())
                .await
                .is_err()
        );
        assert!(
            net.check
                .check("https://93.184.216.34/".into())
                .await
                .is_ok()
        );
    }

    /// Only web pages open; nothing but the browser family exists here; a page
    /// cannot make the bot type into a password field or run a script.
    #[tokio::test]
    async fn only_web_pages_and_only_the_browser_family_are_served() {
        let r = rig("desktop", true, true);
        let me = device(Role::Desktop, true);
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "about:blank"] {
            let res = r.h.handle(&me, RPC_BROWSER, open(bad)).await;
            assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "{bad}");
        }
        let res =
            r.h.handle(
                &me,
                RPC_BROWSER,
                call("eval", &serde_json::json!({"js": "1"})),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        let res =
            r.h.handle(
                &me,
                RPC_BROWSER,
                call("fill", &serde_json::json!({"ref": "e1", "text": "x"})),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN, "password field");
        for method in ["pc.tool", "desktop.windows", "desktop.act"] {
            let res = r.h.handle(&me, method, Value::Null).await;
            assert_eq!(res.unwrap_err().code, ERR_METHOD_NOT_FOUND, "{method}");
        }
        assert!(r.browser.opened.lock().is_empty());
    }

    /// A call that cannot be audited does not run.
    #[tokio::test]
    async fn an_unwritable_audit_log_blocks_the_call() {
        let dir = tempfile::tempdir().unwrap();
        write_pairing(dir.path(), "desktop", true);
        let browser = Arc::new(Browser::default());
        let b = Arc::clone(&browser);
        let h = ServerTargetHandler::new(ServerTargetDeps {
            data: dir.path().to_path_buf(),
            audit: Arc::new(|_, _| Err("disk full".into())),
            redact: Arc::new(str::to_owned),
            browser: Some(Arc::new(move || Arc::clone(&b) as Arc<dyn BrowserDriver>)),
        });
        let res = h
            .handle(
                &device(Role::Desktop, true),
                RPC_BROWSER,
                open("https://example.com/"),
            )
            .await;
        assert_eq!(res.unwrap_err().code, ERR_FORBIDDEN);
        assert!(browser.opened.lock().is_empty());
    }
}
