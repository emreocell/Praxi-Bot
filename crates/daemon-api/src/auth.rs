//! Per-install API token and the request guard (Host / Origin / bearer).

use std::path::{Path, PathBuf};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::ApiError;

/// File name of the token inside the data directory.
pub const TOKEN_FILE_NAME: &str = "api-token";

/// Path of the token file for a data directory.
#[must_use]
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE_NAME)
}

/// A loaded token and whether the file's permissions could be restricted.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    /// The token (64 hex characters).
    pub token: String,
    /// `false` if restricting the file to the current user failed (best effort
    /// on Windows); callers should warn.
    pub restricted: bool,
}

fn valid_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read the token written by the daemon (for clients such as the CLI or the
/// desktop app).
///
/// # Errors
/// I/O errors, or a malformed file.
pub fn read_token(data_dir: &Path) -> std::io::Result<String> {
    let s = std::fs::read_to_string(token_path(data_dir))?;
    let s = s.trim();
    if valid_token(s) {
        Ok(s.to_owned())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "api-token file is malformed",
        ))
    }
}

/// Load the install's token, creating a random one (256 bits) on first use.
///
/// On Unix the file is created `0600`; on Windows it is restricted to the
/// current user with `icacls` (best effort, see [`TokenInfo::restricted`]).
///
/// # Errors
/// I/O errors or no OS randomness.
pub fn load_or_create_token(data_dir: &Path) -> std::io::Result<TokenInfo> {
    if let Ok(token) = read_token(data_dir) {
        return Ok(TokenInfo {
            token,
            restricted: true,
        });
    }
    std::fs::create_dir_all(data_dir)?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut token = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{b:02x}");
    }
    let path = token_path(data_dir);
    write_private(&path, &token)?;
    Ok(TokenInfo {
        token,
        restricted: restrict_permissions(&path),
    })
}

#[cfg(unix)]
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)
}

#[cfg(unix)]
fn restrict_permissions(_path: &Path) -> bool {
    true
}

#[cfg(not(unix))]
fn restrict_permissions(path: &Path) -> bool {
    let Ok(user) = std::env::var("USERNAME") else {
        return false;
    };
    std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(R,W)"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Constant-time equality (length leaks, content does not).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether a `Host` header value names a loopback host (any port).
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let name = if let Some(rest) = h.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, tail)) if tail.is_empty() || is_port(tail.strip_prefix(':')) => inner,
            _ => return false,
        }
    } else {
        match h.rsplit_once(':') {
            Some((n, p)) if is_port(Some(p)) => n,
            Some(_) => return false,
            None => h.as_str(),
        }
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

fn is_port(p: Option<&str>) -> bool {
    p.is_some_and(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

/// Request guard, outermost layer: Host check, Origin check, CORS preflight,
/// then bearer auth (except `GET /v1/health`).
pub(crate) async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // DNS-rebinding protection: only loopback host names.
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(is_loopback_host);
    if !host_ok {
        return ApiError::forbidden("host not allowed").into_response();
    }
    // Browsers always send Origin on cross-origin requests; only the desktop
    // app's origins are accepted.  Non-browser clients send none.
    let origin = match req.headers().get(header::ORIGIN) {
        None => None,
        Some(o) => match o.to_str() {
            Ok(s) if state.origin_allowed(s) => Some(o.clone()),
            _ => return ApiError::forbidden("origin not allowed").into_response(),
        },
    };
    if req.method() == Method::OPTIONS && origin.is_some() {
        return with_cors(StatusCode::NO_CONTENT.into_response(), origin, true);
    }
    let public = req.method() == Method::GET && req.uri().path() == "/v1/health";
    if !public {
        let presented = match bearer(req.headers()) {
            Some(t) => Some(t),
            // A browser `EventSource` cannot set headers: the events route (and
            // only that one) also takes `?token=`.  Host / Origin were already
            // checked above; an `Authorization` header, when present, wins.
            None if is_events_route(&req) => query_token(req.uri().query()),
            None => None,
        };
        let ok = presented.is_some_and(|t| ct_eq(t.as_bytes(), state.token().as_bytes()));
        if !ok {
            return with_cors(ApiError::unauthorized().into_response(), origin, false);
        }
    }
    with_cors(next.run(req).await, origin, false)
}

fn is_events_route(req: &Request) -> bool {
    req.method() == Method::GET && req.uri().path() == "/v1/events"
}

/// The `token` query parameter (raw: tokens are hex, nothing to decode).
fn query_token(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .filter(|t| !t.is_empty())
}

fn with_cors(mut resp: Response, origin: Option<HeaderValue>, preflight: bool) -> Response {
    let Some(origin) = origin else { return resp };
    let h = resp.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    if preflight {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("authorization, content-type, last-event-id"),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        for ok in [
            "localhost",
            "localhost:8080",
            "LOCALHOST:1",
            "127.0.0.1",
            "127.0.0.1:47821",
            "[::1]",
            "[::1]:9",
        ] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in [
            "",
            "evil.com",
            "evil.com:80",
            "localhost.evil.com",
            "127.0.0.1.evil.com",
            "evil.localhost",
            "localhost:",
            "localhost:abc",
            "[::1]x",
            "0.0.0.0",
            "::1",
            "192.168.1.5:80",
        ] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
    }

    #[test]
    fn token_is_created_once_and_reread() {
        let d = tempfile::tempdir().unwrap();
        let a = load_or_create_token(d.path()).unwrap();
        assert!(valid_token(&a.token));
        let b = load_or_create_token(d.path()).unwrap();
        assert_eq!(a.token, b.token);
        assert_eq!(read_token(d.path()).unwrap(), a.token);
        std::fs::write(token_path(d.path()), "short").unwrap();
        assert!(read_token(d.path()).is_err());
        assert_ne!(load_or_create_token(d.path()).unwrap().token, a.token);
    }

    #[test]
    fn query_token_parsing() {
        assert_eq!(query_token(Some("token=abc")), Some("abc"));
        assert_eq!(query_token(Some("run_id=1&token=abc&x=2")), Some("abc"));
        assert_eq!(query_token(Some("token=")), None);
        assert_eq!(query_token(Some("mytoken=abc")), None);
        assert_eq!(query_token(None), None);
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
