// Adapted from vercel-labs/agent-browser (Apache-2.0): browser discovery and launch flags.
//! Locate Chrome/Edge, build launch arguments, and resolve a CDP websocket endpoint.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// Environment variable that overrides browser discovery.
pub const BROWSER_ENV: &str = "PRAXI_BROWSER_PATH";

/// How to launch a browser.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Browser binary; discovered with [`find_browser`] when `None`.
    pub executable: Option<PathBuf>,
    /// Run without a window (`--headless=new`).
    pub headless: bool,
    /// Profile directory. `None` creates an isolated temporary profile that is deleted on close.
    pub user_data_dir: Option<PathBuf>,
    /// Extra command line flags.
    pub args: Vec<String>,
    /// Initial window size.
    pub window_size: (u32, u32),
    /// How long to wait for the debugging endpoint to appear.
    pub startup_timeout: Duration,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            executable: None,
            headless: true,
            user_data_dir: None,
            args: Vec::new(),
            window_size: (1280, 800),
            startup_timeout: Duration::from_secs(20),
        }
    }
}

/// Find an installed Chrome or Edge binary.
pub fn find_browser() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(BROWSER_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    candidate_paths()
        .into_iter()
        .find(|p| p.is_file())
        .ok_or(Error::BrowserNotFound)
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        let roots: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
            .collect();
        for r in &roots {
            v.push(r.join(r"Google\Chrome\Application\chrome.exe"));
        }
        for r in &roots {
            v.push(r.join(r"Microsoft\Edge\Application\msedge.exe"));
        }
    } else if cfg!(target_os = "macos") {
        v.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        v.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
    } else {
        let names = [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "microsoft-edge-stable",
        ];
        let dirs = [
            "/usr/bin",
            "/usr/local/bin",
            "/opt/google/chrome",
            "/snap/bin",
        ];
        for d in dirs {
            for n in names {
                v.push(Path::new(d).join(n));
            }
        }
    }
    v
}

/// Command line flags for a launch (excluding the binary).
pub fn build_args(opts: &LaunchOptions, profile: &Path) -> Vec<String> {
    let mut a = vec![
        "--remote-debugging-port=0".to_string(),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-popup-blocking".into(),
        "--disable-default-apps".into(),
        "--disable-sync".into(),
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        format!(
            "--window-size={},{}",
            opts.window_size.0, opts.window_size.1
        ),
    ];
    if opts.headless {
        a.push("--headless=new".into());
        a.push("--hide-scrollbars".into());
    }
    a.extend(opts.args.iter().cloned());
    a.push("about:blank".into());
    a
}

/// Parse a `DevToolsActivePort` file into a browser websocket URL.
pub fn parse_devtools_active_port(content: &str) -> Option<String> {
    let mut lines = content.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    path.starts_with('/')
        .then(|| format!("ws://127.0.0.1:{port}{path}"))
}

/// Wait for the browser to write `DevToolsActivePort`.
pub(crate) async fn wait_for_endpoint(
    profile: &Path,
    timeout: Duration,
    child: &mut tokio::process::Child,
) -> Result<String> {
    let file = profile.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(c) = tokio::fs::read_to_string(&file).await
            && let Some(url) = parse_devtools_active_port(&c)
        {
            return Ok(url);
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::Launch(format!("browser exited early ({status})")));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Launch(format!(
                "no DevToolsActivePort after {timeout:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Resolve `ws://...`, `http://host:port` or `host:port` into a websocket debugger URL.
pub async fn resolve_endpoint(endpoint: &str) -> Result<String> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_string());
    }
    let hostport = endpoint
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let body = http_get(&hostport, "/json/version").await?;
    let v: Value = serde_json::from_str(&body)?;
    v.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Launch("endpoint has no webSocketDebuggerUrl".into()))
}

/// Minimal HTTP/1.1 GET over a raw socket (avoids an HTTP client dependency).
async fn http_get(hostport: &str, path: &str) -> Result<String> {
    let fut = async {
        let mut s = TcpStream::connect(hostport).await?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
    };
    let raw = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .map_err(|_| Error::Timeout(format!("GET {hostport}{path}")))??;
    raw.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .ok_or_else(|| Error::Launch("malformed HTTP response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_port_file() {
        let url = parse_devtools_active_port("9222\n/devtools/browser/abc-123\n").unwrap();
        assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/abc-123");
        assert!(parse_devtools_active_port("9222\n").is_none());
        assert!(parse_devtools_active_port("x\n/y").is_none());
    }

    #[test]
    fn args_are_isolated_and_headless() {
        let o = LaunchOptions {
            args: vec!["--foo".into()],
            ..Default::default()
        };
        let a = build_args(&o, Path::new("/tmp/p"));
        assert!(a.contains(&"--remote-debugging-port=0".to_string()));
        assert!(a.iter().any(|x| x.starts_with("--user-data-dir=")));
        assert!(a.contains(&"--headless=new".to_string()));
        assert!(a.contains(&"--foo".to_string()));
        assert_eq!(a.last().unwrap(), "about:blank");
        let headed = build_args(
            &LaunchOptions {
                headless: false,
                ..Default::default()
            },
            Path::new("/tmp/p"),
        );
        assert!(!headed.contains(&"--headless=new".to_string()));
    }

    #[tokio::test]
    async fn resolves_http_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut b = [0u8; 512];
            let _ = s.read(&mut b).await.unwrap();
            let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        let url = resolve_endpoint(&format!("http://{addr}")).await.unwrap();
        assert_eq!(url, "ws://127.0.0.1:1/devtools/browser/x");
        assert_eq!(resolve_endpoint("ws://h:1/x").await.unwrap(), "ws://h:1/x");
    }
}
