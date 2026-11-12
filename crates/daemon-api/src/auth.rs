//! Per-install API token and the request guard (Host / Origin / bearer).

use std::path::{Path, PathBuf};

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::ApiError;

/// File name of the token inside the data directory.
pub const TOKEN_FILE_NAME: &str = "api-token";

/// Authority required by a route; declared alongside its handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Privilege {
    Public,
    Cli,
    Management,
}

/// Peer scope is mandatory in each route declaration. `HostManagement` is
/// opt-in for a specifically authorized paired Desktop key, never for phones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerScope {
    Deny,
    Allow,
    HostManagement,
}

/// Ingress-created authority. Never inferred from request bodies or headers
/// claiming to be a peer. This also scopes live and replayed SSE events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Principal {
    LocalCli,
    LocalManagement,
    Peer { host_management: bool },
}

impl Principal {
    pub(crate) fn is_local_management(self) -> bool {
        self == Self::LocalManagement
    }
    pub(crate) fn may_manage_host(self) -> bool {
        matches!(
            self,
            Self::LocalManagement
                | Self::Peer {
                    host_management: true
                }
        )
    }
    pub(crate) fn may_receive_decisions(self) -> bool {
        self != Self::LocalCli
    }
}

pub(crate) type RouteRule = (&'static str, &'static str, Privilege, PeerScope, PeerScope);

pub(crate) fn route_rule(req: &Request) -> Option<&'static RouteRule> {
    let path = req.extensions().get::<MatchedPath>()?.as_str();
    let method = if req.method() == Method::HEAD {
        "GET"
    } else {
        req.method().as_str()
    };
    crate::ROUTE_PRIVILEGES
        .iter()
        .find(|(p, m, _, _, _)| *p == path && *m == method)
}

/// Random memory-only principal for this launch. Never persist it or pass it
/// through environment variables or command-line arguments.
///
/// # Errors
/// The OS random source is unavailable.
pub fn launch_token() -> std::io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    }))
}

/// Consume the fixed-size bootstrap frame before any tool children start.
/// Reading a frame rather than an environment variable avoids inherited secrets.
///
/// # Errors
/// Missing, short or malformed bootstrap input. No fallback to disk authority.
pub fn read_launch_token(reader: &mut impl std::io::Read) -> std::io::Result<String> {
    let mut frame = [0u8; 65];
    reader.read_exact(&mut frame)?;
    let token = std::str::from_utf8(&frame[..64]).unwrap_or_default();
    if frame[64] != b'\n' || !valid_token(token) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid UI bootstrap frame",
        ));
    }
    Ok(token.to_owned())
}

/// Consume and seal the desktop's anonymous stdin channel before tools start.
///
/// # Errors
/// Invalid input or inability to prevent descriptor inheritance. Startup fails.
pub fn bootstrap_from_stdin() -> std::io::Result<String> {
    process_guard::seal_bootstrap_stdin()?;
    read_launch_token(&mut std::io::stdin().lock())
}

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
/// On Unix the file is created `0600`; an existing file is checked through
/// the open descriptor (owner = the data directory's owner, no group / other
/// bits) and repaired to `0600`, or rejected.  On Windows the file is created
/// empty, restricted to the current user with `icacls`, and only then filled;
/// an existing file gets the same restriction re-applied.
/// [`TokenInfo::restricted`] reports what really happened.
///
/// # Errors
/// I/O errors, no OS randomness, or an existing token file that is a symlink,
/// owned by someone else, or whose permissions cannot be repaired.
pub fn load_or_create_token(data_dir: &Path) -> std::io::Result<TokenInfo> {
    if let Some(info) = existing_token(data_dir)? {
        return Ok(info);
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
    let restricted = write_private(&path, &token)?;
    Ok(TokenInfo { token, restricted })
}

/// The token of an existing, usable file with its permissions verified;
/// `None` when there is none or it is malformed (a new one replaces it).
fn existing_token(data_dir: &Path) -> std::io::Result<Option<TokenInfo>> {
    let path = token_path(data_dir);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "api-token is not a regular file",
            ));
        }
        Ok(_) => {}
    }
    let (content, restricted) = read_verified(data_dir, &path)?;
    let t = content.trim();
    Ok(valid_token(t).then(|| TokenInfo {
        token: t.to_owned(),
        restricted,
    }))
}

#[cfg(unix)]
fn read_verified(data_dir: &Path, path: &Path) -> std::io::Result<(String, bool)> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let deny = |m: &str| std::io::Error::new(std::io::ErrorKind::PermissionDenied, m.to_owned());
    // The data directory must not be world writable (anyone could swap the
    // file); group-writable stays allowed (umask 002 is the common default).
    if std::fs::metadata(data_dir)?.mode() & 0o002 != 0 {
        return Err(deny("data directory is world writable"));
    }
    let flags =
        rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC;
    // No symlink is followed (O_NOFOLLOW, atomic with the open), no FIFO blocks.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(checked_open_flags(flags.bits())?)
        .open(path)?;
    let meta = f.metadata()?;
    let euid = rustix::process::geteuid().as_raw();
    if !meta.is_file() {
        return Err(deny("api-token is not a regular file"));
    }
    if meta.nlink() != 1 {
        return Err(deny("api-token has more than one hard link"));
    }
    if meta.uid() != euid {
        return Err(deny("api-token is not owned by the current user"));
    }
    if meta.mode() & 0o077 != 0 {
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        if f.metadata()?.mode() & 0o077 != 0 {
            return Err(deny("api-token permissions could not be restricted"));
        }
    }
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok((s, true))
}

#[cfg(unix)]
fn checked_open_flags(bits: u32) -> std::io::Result<i32> {
    i32::try_from(bits).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "api-token open flags cannot be represented by the operating system",
        )
    })
}

/// Windows: the ACL is restricted *before* the secret is read; failing to do
/// so is an error (no token is served from a file we could not protect).
#[cfg(not(unix))]
fn read_verified(_data_dir: &Path, path: &Path) -> std::io::Result<(String, bool)> {
    restrict_permissions(path)?;
    Ok((std::fs::read_to_string(path)?, true))
}

#[cfg(unix)]
fn write_private(path: &Path, content: &str) -> std::io::Result<bool> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    Ok(true)
}

/// Create the file empty, restrict it, then write the secret: the token never
/// sits in a file with inherited (wider) permissions.  If the restriction
/// fails the file is removed and the error returned (startup fails visibly).
#[cfg(not(unix))]
fn write_private(path: &Path, content: &str) -> std::io::Result<bool> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(e) = restrict_permissions(path) {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    f.write_all(content.as_bytes())?;
    Ok(true)
}

/// SID of the current user (`whoami /user`), not a spoofable env var.
#[cfg(not(unix))]
fn current_user_sid() -> std::io::Result<String> {
    let out = std::process::Command::new(system_command("whoami.exe")?)
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .rsplit(',')
        .next()
        .map(|f| f.trim().trim_matches('"').to_owned())
        .filter(|sid| out.status.success() && sid.starts_with("S-1-"))
        .ok_or_else(|| std::io::Error::other("cannot determine the current user's SID"))
}

/// Security utilities must never be resolved through the working directory or PATH.
#[cfg(not(unix))]
fn system_command(name: &str) -> std::io::Result<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| std::io::Error::other("SystemRoot must be an absolute path"))?;
    Ok(root.join("System32").join(name))
}

#[cfg(not(unix))]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    let sid = current_user_sid()?;
    let icacls = |args: &[&std::ffi::OsStr]| -> std::io::Result<()> {
        let st = std::process::Command::new(system_command("icacls.exe")?)
            .arg(path)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        st.success().then_some(()).ok_or_else(|| {
            std::io::Error::other("icacls failed: api-token permissions could not be restricted")
        })
    };
    let grant = std::ffi::OsString::from(format!("*{sid}:(R,W)"));
    // Drop explicit entries first (a pre-existing file may carry any), then
    // the inherited ones, then grant the current user alone.
    icacls(&["/reset".as_ref()])?;
    icacls(&[
        "/inheritance:r".as_ref(),
        "/grant:r".as_ref(),
        grant.as_ref(),
    ])
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
pub(crate) async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
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
    // `GET /v1/health`, and the webhooks, which carry an HMAC of the body
    // instead of the bearer token (the route verifies it).
    let required = route_rule(&req).map(|rule| rule.2);
    let public = required == Some(Privilege::Public);
    if !public {
        let presented = match bearer(req.headers()) {
            Some(t) => Some(t),
            // A browser `EventSource` cannot set headers: the events route (and
            // only that one) also takes `?token=`.  Host / Origin were already
            // checked above; an `Authorization` header, when present, wins.
            None if is_events_route(&req) => query_token(req.uri().query()),
            None => None,
        };
        // High authority is accepted ONLY in a header, never in a query URL.
        // The on-disk token cannot be promoted by configuration equality.
        let management = bearer(req.headers()).is_some_and(|t| {
            state.management_token().is_some_and(|secret| {
                !secret.is_empty()
                    && secret != state.token()
                    && ct_eq(t.as_bytes(), secret.as_bytes())
            })
        });
        let cli = presented.is_some_and(|t| ct_eq(t.as_bytes(), state.token().as_bytes()));
        if !cli && !management {
            return with_cors(ApiError::unauthorized().into_response(), origin, false);
        }
        if required.is_none() || (required == Some(Privilege::Management) && !management) {
            return with_cors(ApiError::forbidden(
                "desktop-only operation: the api-token credential has no management, approval or control authority; use the desktop app (stop a manually started daemon and let the desktop start it), or an authorized paired device",
            ).into_response(), origin, false);
        }
        req.extensions_mut().insert(if management {
            Principal::LocalManagement
        } else {
            Principal::LocalCli
        });
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

    #[cfg(unix)]
    #[test]
    fn loose_token_file_is_repaired_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let a = load_or_create_token(d.path()).unwrap();
        let p = token_path(d.path());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let b = load_or_create_token(d.path()).unwrap();
        assert_eq!(a.token, b.token);
        assert!(b.restricted);
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_token_and_writable_data_dir_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        load_or_create_token(d.path()).unwrap();
        let link = d.path().join("other-name");
        std::fs::hard_link(token_path(d.path()), &link).unwrap();
        assert!(load_or_create_token(d.path()).is_err(), "nlink > 1");
        std::fs::remove_file(&link).unwrap();
        assert!(load_or_create_token(d.path()).is_ok());
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            load_or_create_token(d.path()).is_err(),
            "world-writable dir"
        );
    }

    #[cfg(unix)]
    #[test]
    fn token_symlink_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("elsewhere");
        std::fs::write(&target, "a".repeat(64)).unwrap();
        std::os::unix::fs::symlink(&target, token_path(d.path())).unwrap();
        assert!(load_or_create_token(d.path()).is_err());
    }

    /// An existing token file with a wider ACL is tightened (SEC-14).
    #[cfg(windows)]
    #[test]
    fn existing_token_file_acl_is_restricted() {
        let d = tempfile::tempdir().unwrap();
        let p = token_path(d.path());
        std::fs::write(&p, "ab".repeat(32)).unwrap();
        let st = std::process::Command::new(system_command("icacls.exe").unwrap())
            .arg(&p)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(st.status.success());
        let info = load_or_create_token(d.path()).unwrap();
        assert_eq!(info.token, "ab".repeat(32));
        assert!(info.restricted, "restriction must succeed and be reported");
        let acl = std::process::Command::new(system_command("icacls.exe").unwrap())
            .arg(&p)
            .output()
            .unwrap();
        let acl = String::from_utf8_lossy(&acl.stdout).to_lowercase();
        assert!(!acl.contains("everyone"), "{acl}");
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

    #[cfg(unix)]
    #[test]
    fn unsupported_open_flag_bits_are_rejected_instead_of_dropped() {
        assert_eq!(
            checked_open_flags(u32::MAX).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        let flags = rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC;
        assert_eq!(
            u32::try_from(checked_open_flags(flags.bits()).unwrap()).unwrap(),
            flags.bits()
        );
    }

    #[cfg(windows)]
    #[test]
    fn acl_utilities_resolve_to_absolute_system_paths() {
        for executable in ["whoami.exe", "icacls.exe"] {
            let path = system_command(executable).unwrap();
            assert!(path.is_absolute());
            assert_eq!(path.file_name().unwrap(), executable);
            assert_eq!(path.parent().unwrap().file_name().unwrap(), "System32");
        }
    }
}
