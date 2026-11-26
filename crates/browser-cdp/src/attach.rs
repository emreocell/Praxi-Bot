//! Attaching to a browser the owner already has open ("use this one").
//!
//! The owner starts Chrome or Edge with `--remote-debugging-port=9222` (or switches remote
//! debugging on in `chrome://inspect`) and the assistant connects to it instead of starting a
//! browser of its own. Such a browser holds the owner's signed-in sessions, so two rules live
//! here, in pure code with tests:
//!
//! * the endpoint must be **this computer** (loopback): a debugging port on another machine
//!   is never connected to, whatever the model or a page says;
//! * discovery only reads the places a browser announces itself (well-known ports and the
//!   `DevToolsActivePort` file of the default profile); it never scans a network.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::browser::Browser;
use crate::error::{Error, Result};
use crate::launch::{http_get_at, parse_devtools_active_port};
use crate::page::Page;

/// Ports an owner's browser is commonly started on (`--remote-debugging-port`).
pub const COMMON_DEBUG_PORTS: &[u16] = &[9222, 9223, 9224];

/// Refuse an endpoint that is not on this computer.
///
/// Accepts `ws://host:port/...`, `http://host:port` and `host:port`, where `host` is
/// `localhost`, an IPv4 address in `127.0.0.0/8` or `[::1]`. Only plain `http` / `ws`: a
/// debugging port on this computer does not speak TLS, and the connection code below only
/// handles those two.
pub fn loopback_endpoint(endpoint: &str) -> Result<()> {
    let refuse = |why: &str| {
        Err(Error::Launch(format!(
            "refused: {why}. Only a browser on this computer can be attached (127.0.0.1, localhost or [::1])"
        )))
    };
    let e = endpoint.trim();
    if e.is_empty() || e.chars().any(|c| c.is_control() || c == ' ') {
        return refuse("the endpoint is empty or malformed");
    }
    let with_scheme = if e.contains("://") {
        e.to_owned()
    } else {
        format!("http://{e}")
    };
    let Ok(url) = url::Url::parse(&with_scheme) else {
        return refuse("the endpoint is not a valid address");
    };
    if !matches!(url.scheme(), "http" | "ws") {
        return refuse("the endpoint must be http or ws");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return refuse("the endpoint must not carry a user name");
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !loopback {
        return refuse("the endpoint is not on this computer");
    }
    if url.port().is_none() {
        return refuse("the endpoint needs the debugging port");
    }
    Ok(())
}

/// Folders where an owner's browser writes `DevToolsActivePort` while remote debugging is on.
#[must_use]
pub fn owner_profile_dirs(home: Option<PathBuf>, local_app_data: Option<PathBuf>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = home {
        for rel in [
            ".config/google-chrome",
            ".config/chromium",
            ".config/microsoft-edge",
            "Library/Application Support/Google/Chrome",
            "Library/Application Support/Microsoft Edge",
            "snap/chromium/common/chromium",
        ] {
            v.push(home.join(rel));
        }
    }
    if let Some(local) = local_app_data {
        for rel in [
            r"Google\Chrome\User Data",
            r"Microsoft\Edge\User Data",
            r"Chromium\User Data",
        ] {
            v.push(local.join(rel));
        }
    }
    v
}

/// Browser endpoints the owner's computer announces: `DevToolsActivePort` files first (they
/// name the exact browser), then the common debugging ports. Nothing is connected to here.
pub async fn discover_owner_endpoints() -> Vec<String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let mut found = Vec::new();
    for dir in owner_profile_dirs(home, local) {
        if let Ok(text) = tokio::fs::read_to_string(dir.join("DevToolsActivePort")).await
            && let Some(ws) = parse_devtools_active_port(&text)
        {
            found.push(ws);
        }
    }
    for port in COMMON_DEBUG_PORTS {
        found.push(format!("http://127.0.0.1:{port}"));
    }
    found
}

/// The loopback addresses `url` names, resolved ONCE (a name is looked up here and never again,
/// so it cannot answer with another address the second time). Every address that is not
/// loopback is dropped; none left is a refusal.
async fn pinned_addrs(url: &url::Url) -> Result<Vec<SocketAddr>> {
    let port = url
        .port()
        .ok_or_else(|| Error::Launch("refused: the endpoint needs the debugging port".into()))?;
    let found: Vec<SocketAddr> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(url::Host::Domain(d)) => tokio::net::lookup_host((d, port))
            .await
            .map_err(|e| Error::Launch(format!("cannot resolve {d}: {e}")))?
            .collect(),
        None => Vec::new(),
    };
    let loopback: Vec<SocketAddr> = found.into_iter().filter(|a| a.ip().is_loopback()).collect();
    if loopback.is_empty() {
        return Err(Error::Launch(
            "refused: the endpoint does not resolve to this computer".into(),
        ));
    }
    Ok(loopback)
}

/// `ws` with its host replaced by the already checked address, so connecting resolves nothing.
fn pinned_ws(ws: &url::Url, addr: SocketAddr) -> Result<String> {
    let mut pinned = ws.clone();
    pinned
        .set_ip_host(addr.ip())
        .map_err(|()| Error::Launch("refused: the endpoint cannot be pinned".into()))?;
    Ok(pinned.to_string())
}

/// Connect to the browser at `endpoint` (loopback only, see [`loopback_endpoint`]).
///
/// The host is resolved once and every address used is checked to be loopback. For an
/// `http://` endpoint the browser's own `/json/version` names the websocket to connect to; that
/// address is validated exactly like the endpoint (loopback, `ws`, the same port) before it is
/// used, so a debugging port that answers with another machine's address is never followed.
pub async fn connect_loopback(endpoint: &str) -> Result<Browser> {
    loopback_endpoint(endpoint)?;
    let e = endpoint.trim();
    let url = url::Url::parse(&if e.contains("://") {
        e.to_owned()
    } else {
        format!("http://{e}")
    })
    .map_err(|_| Error::Launch("refused: the endpoint is not a valid address".into()))?;
    let addrs = pinned_addrs(&url).await?;
    if url.scheme() == "ws" {
        let addr = addrs[0];
        return Browser::connect(&pinned_ws(&url, addr)?).await;
    }
    let host_header = format!(
        "{}:{}",
        url.host_str().unwrap_or("localhost"),
        url.port().unwrap_or(0)
    );
    let mut last: Option<Error> = None;
    for addr in addrs {
        let body = match http_get_at(addr, &host_header, "/json/version").await {
            Ok(b) => b,
            Err(err) => {
                last = Some(err);
                continue;
            }
        };
        let version: serde_json::Value = serde_json::from_str(&body)?;
        let Some(raw_ws) = version["webSocketDebuggerUrl"].as_str() else {
            return Err(Error::Launch("endpoint has no webSocketDebuggerUrl".into()));
        };
        // Same rules as the endpoint itself, plus: it must be the port that was asked.
        loopback_endpoint(raw_ws)?;
        let ws = url::Url::parse(raw_ws)
            .map_err(|_| Error::Launch("refused: webSocketDebuggerUrl is not an address".into()))?;
        if ws.scheme() != "ws" || ws.port() != url.port() {
            return Err(Error::Launch(
                "refused: the browser named a different debugging address than the one asked"
                    .into(),
            ));
        }
        let ws_addrs = pinned_addrs(&ws).await?;
        // Prefer the address that just answered; never one that was not checked.
        let target = ws_addrs
            .iter()
            .copied()
            .find(|a| *a == addr)
            .unwrap_or(ws_addrs[0]);
        return Browser::connect(&pinned_ws(&ws, target)?).await;
    }
    Err(last.unwrap_or_else(|| Error::Launch("no address to connect to".into())))
}

/// Connect to the first of `candidates` that answers, within a short deadline each. A candidate
/// that is not a loopback address (a stale or hostile `DevToolsActivePort`) is skipped, not fatal.
pub async fn connect_first(candidates: &[String]) -> Result<(Browser, String)> {
    let mut last: Option<Error> = None;
    for c in candidates {
        if let Err(e) = loopback_endpoint(c) {
            last = Some(e);
            continue;
        }
        match tokio::time::timeout(Duration::from_secs(5), connect_loopback(c)).await {
            Ok(Ok(b)) => return Ok((b, c.clone())),
            Ok(Err(e)) => last = Some(e),
            Err(_) => last = Some(Error::Timeout(format!("connecting to {c}"))),
        }
    }
    Err(last.unwrap_or_else(|| Error::Launch("no browser endpoint to try".into())))
}

impl Browser {
    /// The tab the owner is looking at: the first tab whose page reports itself visible, else
    /// the first tab. Reads `document.visibilityState` through a short-lived session that
    /// enables no domains and changes nothing on the page.
    pub async fn visible_page(&self) -> Result<Page> {
        let pages = self.pages().await?;
        for t in pages.iter().take(16) {
            let Ok(r) = self
                .client()
                .send(
                    "Target.attachToTarget",
                    serde_json::json!({"targetId": t.target_id, "flatten": true}),
                )
                .await
            else {
                continue;
            };
            let Some(sid) = r["sessionId"].as_str().map(str::to_owned) else {
                continue;
            };
            let state = self
                .client()
                .send_with_timeout(
                    Some(&sid),
                    "Runtime.evaluate",
                    serde_json::json!({"expression": "document.visibilityState", "returnByValue": true}),
                    Duration::from_secs(2),
                )
                .await;
            let _ = self
                .client()
                .send(
                    "Target.detachFromTarget",
                    serde_json::json!({"sessionId": sid}),
                )
                .await;
            if state.is_ok_and(|v| v["result"]["value"].as_str() == Some("visible")) {
                return self.attach(&t.target_id).await;
            }
        }
        self.first_page().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_computer_can_be_attached() {
        for ok in [
            "http://127.0.0.1:9222",
            "127.0.0.1:9222",
            "localhost:9222",
            "http://localhost:9222/json/version",
            "ws://127.0.0.1:9222/devtools/browser/abc",
            "ws://[::1]:9222/devtools/browser/abc",
            "http://127.0.0.2:9333",
        ] {
            assert!(loopback_endpoint(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "http://192.168.1.5:9222",
            "http://10.0.0.1:9222",
            "http://example.com:9222",
            "http://localhost.evil.example:9222",
            "http://127.0.0.1.evil.example:9222",
            "http://user:pw@127.0.0.1:9222",
            "http://127.0.0.1",
            "file:///etc/passwd",
            // TLS schemes are not spoken by a local debugging port and are not handled below.
            "https://127.0.0.1:9222",
            "wss://127.0.0.1:9222/devtools/browser/abc",
            "http://[::ffff:10.0.0.1]:9222",
            "127.0.0.1:9222 evil.example",
            "http://0.0.0.0:9222",
        ] {
            assert!(loopback_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn profile_dirs_cover_the_usual_browsers_per_os_root() {
        let dirs = owner_profile_dirs(
            Some(PathBuf::from("/home/o")),
            Some(PathBuf::from(r"C:\Users\o\AppData\Local")),
        );
        assert!(dirs.contains(&PathBuf::from("/home/o/.config/google-chrome")));
        assert!(dirs.contains(&PathBuf::from("/home/o/.config/microsoft-edge")));
        assert!(
            dirs.iter().any(|d| d.to_string_lossy().contains("Edge")
                && d.to_string_lossy().contains("User Data"))
        );
        assert_eq!(owner_profile_dirs(None, None), Vec::<PathBuf>::new());
    }

    /// A fake debugging port: answers every request with `/json/version` naming `ws`.
    async fn fake_version_server(ws: impl Fn(u16) -> String + Send + 'static) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let body = format!("{{\"webSocketDebuggerUrl\":\"{}\"}}", ws(port));
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    fn refusal(r: Result<Browser>) -> String {
        match r {
            Ok(_) => panic!("must not connect"),
            Err(e) => e.to_string(),
        }
    }

    #[tokio::test]
    async fn a_debugging_port_cannot_send_the_assistant_to_another_address() {
        // The browser named another machine, another port, a TLS address, a name that is not
        // loopback: none is connected to.
        for hostile in [
            "ws://192.0.2.7:{port}/devtools/browser/x",
            "ws://10.0.0.5:{port}/devtools/browser/x",
            "ws://evil.example:{port}/devtools/browser/x",
            "wss://127.0.0.1:{port}/devtools/browser/x",
            "ws://127.0.0.1:1/devtools/browser/x",
            "ws://user:pw@127.0.0.1:{port}/devtools/browser/x",
            "http://127.0.0.1:{port}/devtools/browser/x",
        ] {
            let template = hostile.to_owned();
            let port =
                fake_version_server(move |p| template.replace("{port}", &p.to_string())).await;
            let e = refusal(connect_loopback(&format!("http://127.0.0.1:{port}")).await);
            assert!(e.contains("refused"), "{hostile}: {e}");
        }
    }

    #[tokio::test]
    async fn a_loopback_debugging_address_is_followed_only_after_the_same_checks() {
        // The same host and port is accepted by the checks (there is no websocket behind the
        // fake, so the connection itself fails: that failure is not a refusal).
        let port = fake_version_server(|p| format!("ws://127.0.0.1:{p}/devtools/browser/x")).await;
        let e = refusal(connect_loopback(&format!("http://127.0.0.1:{port}")).await);
        assert!(!e.contains("refused"), "{e}");
        // `localhost` is resolved once and only a loopback answer is used.
        let e = refusal(connect_loopback(&format!("http://localhost:{port}")).await);
        assert!(!e.contains("refused"), "{e}");
    }

    #[tokio::test]
    async fn an_invalid_candidate_is_skipped_not_fatal() {
        let port = fake_version_server(|p| format!("ws://127.0.0.1:{}/x", p.wrapping_add(1))).await;
        let list = vec![
            "http://192.168.1.5:9222".to_owned(),
            "https://127.0.0.1:9222".to_owned(),
            format!("http://127.0.0.1:{port}"),
        ];
        // The last candidate is reached (its own refusal is the error), not the first one's.
        let e = connect_first(&list)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("different debugging address"), "{e}");
    }
}
