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

use std::path::PathBuf;
use std::time::Duration;

use crate::browser::Browser;
use crate::error::{Error, Result};
use crate::launch::{parse_devtools_active_port, resolve_endpoint};
use crate::page::Page;

/// Ports an owner's browser is commonly started on (`--remote-debugging-port`).
pub const COMMON_DEBUG_PORTS: &[u16] = &[9222, 9223, 9224];

/// Refuse an endpoint that is not on this computer.
///
/// Accepts `ws://host:port/...`, `http://host:port` and `host:port`, where `host` is
/// `localhost`, an IPv4 address in `127.0.0.0/8` or `[::1]`.
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
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
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

/// Connect to the first of `candidates` that answers, within a short deadline each.
pub async fn connect_first(candidates: &[String]) -> Result<(Browser, String)> {
    let mut last: Option<Error> = None;
    for c in candidates {
        loopback_endpoint(c)?;
        match tokio::time::timeout(Duration::from_secs(5), async {
            // Resolve first, so a closed port is a quick error and not a hang.
            resolve_endpoint(c).await?;
            Browser::connect(c).await
        })
        .await
        {
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
}
