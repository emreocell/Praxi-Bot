//! Catches an OAuth redirect on THIS computer so a daemon on another one can
//! finish the sign-in.
//!
//! When the daemon runs on a server, the browser the owner signs in with is
//! here, and the authorization server sends it back to
//! `http://127.0.0.1:<port>/callback` on this machine, where nothing used to
//! listen. The app now opens that port first ([`listen`]), tells the daemon
//! which port to use, and waits ([`wait`]) for the one redirect that carries
//! the daemon's `state`. The page then hands the full address to the daemon
//! over the paired channel it already has; the daemon checks the address and
//! `state` again before it trusts it.
//!
//! The listener is bound to `127.0.0.1` only, serves one purpose, ignores
//! anything that is not the expected redirect (a stray local request cannot
//! use it up, and after [`MAX_STRAY_REQUESTS`] of them the wait ends so a local
//! process cannot keep the port busy), and goes away after the first good
//! redirect, a cancel or the time limit. No code or token is logged or kept.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// Longest request head read from one connection.
const MAX_HEAD: usize = 8 * 1024;
/// How long one connection may take to send its request line and headers.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a sign-in is waited for (the daemon waits the same five minutes).
pub const WAIT_LIMIT: Duration = Duration::from_secs(5 * 60);

/// Requests that are not the expected redirect (wrong path or `state`, a slow
/// or foreign connection) the listener puts up with before it gives up.
pub const MAX_STRAY_REQUESTS: usize = 50;

/// What [`listen`] reports: the handle to wait on and the port to give the daemon.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Listening {
    pub id: u64,
    pub port: u16,
}

struct Slot {
    id: u64,
    listener: Option<TcpListener>,
    cancel: Arc<Notify>,
}

/// The one listener there can be: a new sign-in replaces an old one.
static SLOT: Mutex<Option<Slot>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn slot() -> std::sync::MutexGuard<'static, Option<Slot>> {
    SLOT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open a one-shot listener on a free `127.0.0.1` port.
///
/// # Errors
/// `bind_failed` when no port could be opened.
pub async fn listen() -> Result<Listening, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "bind_failed".to_owned())?;
    let port = listener
        .local_addr()
        .map_err(|_| "bind_failed".to_owned())?
        .port();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let previous = slot().replace(Slot {
        id,
        listener: Some(listener),
        cancel: Arc::new(Notify::new()),
    });
    if let Some(old) = previous {
        old.cancel.notify_one();
    }
    Ok(Listening { id, port })
}

/// Stop listening for `id` (no-op when it is not the current one).
pub fn cancel(id: u64) {
    let mut guard = slot();
    if guard.as_ref().is_some_and(|s| s.id == id)
        && let Some(old) = guard.take()
    {
        old.cancel.notify_one();
    }
}

/// Wait for the redirect carrying `state` and return its address
/// (`http://127.0.0.1:<port>/callback?...`).
///
/// # Errors
/// `not_listening` (unknown or already used id), `cancelled`, `timeout`,
/// `too_many_requests` (more than [`MAX_STRAY_REQUESTS`] strays).
pub async fn wait(id: u64, state: &str) -> Result<String, String> {
    let (listener, cancel) = {
        let mut guard = slot();
        match guard.as_mut() {
            Some(s) if s.id == id => match s.listener.take() {
                Some(listener) => (listener, Arc::clone(&s.cancel)),
                None => return Err("not_listening".into()),
            },
            _ => return Err("not_listening".into()),
        }
    };
    let port = listener.local_addr().map_err(|_| "not_listening")?.port();
    let outcome = tokio::select! {
        found = accept_redirect(&listener, port, state, MAX_STRAY_REQUESTS) => found,
        () = cancel.notified() => Err("cancelled".to_owned()),
        () = tokio::time::sleep(WAIT_LIMIT) => Err("timeout".to_owned()),
    };
    // One-shot: whatever the outcome, the port is closed and the slot free.
    drop(listener);
    let mut guard = slot();
    if guard.as_ref().is_some_and(|s| s.id == id) {
        *guard = None;
    }
    outcome
}

async fn accept_redirect(
    listener: &TcpListener,
    port: u16,
    state: &str,
    max_strays: usize,
) -> Result<String, String> {
    let mut strays = 0usize;
    loop {
        let (mut socket, peer) = listener
            .accept()
            .await
            .map_err(|_| "not_listening".to_owned())?;
        let answer = if peer.ip().is_loopback() {
            match tokio::time::timeout(READ_TIMEOUT, read_head(&mut socket)).await {
                Ok(Some(head)) => redirect_from_request(&head, port, state).ok(),
                _ => None,
            }
        } else {
            None
        };
        if let Some(url) = answer {
            respond(&mut socket, "200 OK", DONE_PAGE).await;
            return Ok(url);
        }
        respond(&mut socket, "400 Bad Request", REFUSED_PAGE).await;
        strays += 1;
        if strays >= max_strays {
            // Only the count is logged, never an address or a query.
            support_logs::tracing::warn!(
                target: "praxi_support",
                reason = "oauth_relay_stray_requests",
                count = strays
            );
            return Err("too_many_requests".into());
        }
    }
}

async fn read_head(socket: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_HEAD {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Praxi</title>\
<p>Signed in. You can close this tab and go back to Praxi.</p>\
<p>Giriş tamamlandı. Bu sekmeyi kapatıp Praxi'ye dönebilirsiniz.</p>";
const REFUSED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Praxi</title>\
<p>This is not the sign-in Praxi is waiting for. Go back to Praxi and try again.</p>\
<p>Praxi'nin beklediği giriş bu değil. Praxi'ye dönüp yeniden deneyin.</p>";

async fn respond(socket: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(reply.as_bytes()).await;
    let _ = socket.shutdown().await;
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// The redirect address in `head` (a request head), when it is `GET /callback`
/// to this listener's own host and port and carries `expected_state`.
///
/// The `Host` header is checked so a web page cannot reach the listener under
/// another name (DNS rebinding); the `state` is compared before anything is
/// returned, so only the sign-in the daemon started can use the listener.
pub(crate) fn redirect_from_request(
    head: &str,
    port: u16,
    expected_state: &str,
) -> Result<String, ()> {
    if expected_state.is_empty() {
        return Err(());
    }
    let mut lines = head.split("\r\n");
    let target = lines
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .ok_or(())?;
    let host_ok = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .any(|(_, value)| {
            let value = value.trim().to_ascii_lowercase();
            value == format!("127.0.0.1:{port}") || value == format!("localhost:{port}")
        });
    if !host_ok {
        return Err(());
    }
    let query = target.strip_prefix("/callback?").ok_or(())?;
    // Raw comparison: the daemon's state is URL-safe, and the daemon decodes
    // and compares it again.
    if query_param(query, "state") != Some(expected_state) {
        return Err(());
    }
    Ok(format!("http://127.0.0.1:{port}/callback?{query}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listener slot is process-wide: tests that use it run one at a time.
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Until the spawned `wait` for `id` has taken its listener.
    async fn until_waiting(id: u64) {
        for _ in 0..200 {
            if slot()
                .as_ref()
                .is_some_and(|s| s.id == id && s.listener.is_none())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("wait did not start");
    }

    fn head(target: &str, host: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: x\r\n\r\n")
    }

    #[test]
    fn only_the_expected_redirect_is_forwarded() {
        let ok = redirect_from_request(
            &head("/callback?code=c1&state=st-1", "127.0.0.1:4000"),
            4000,
            "st-1",
        );
        assert_eq!(
            ok.unwrap(),
            "http://127.0.0.1:4000/callback?code=c1&state=st-1"
        );
        // A provider that refuses still comes back with the state: forwarded, the daemon reports it.
        assert!(
            redirect_from_request(
                &head("/callback?error=access_denied&state=st-1", "localhost:4000"),
                4000,
                "st-1"
            )
            .is_ok()
        );
        for (target, host, state) in [
            ("/callback?code=c1&state=other", "127.0.0.1:4000", "st-1"),
            ("/callback?code=c1", "127.0.0.1:4000", "st-1"),
            ("/other?code=c1&state=st-1", "127.0.0.1:4000", "st-1"),
            ("/callback", "127.0.0.1:4000", "st-1"),
            // DNS rebinding: another name or port in `Host`.
            ("/callback?code=c1&state=st-1", "evil.example:4000", "st-1"),
            ("/callback?code=c1&state=st-1", "127.0.0.1:4001", "st-1"),
            // No state to compare against: nothing is accepted.
            ("/callback?code=c1&state=", "127.0.0.1:4000", ""),
        ] {
            assert!(
                redirect_from_request(&head(target, host), 4000, state).is_err(),
                "{target} {host}"
            );
        }
        assert!(
            redirect_from_request("POST /callback?state=st-1 HTTP/1.1\r\n\r\n", 4000, "st-1")
                .is_err()
        );
    }

    async fn get(port: u16, target: &str) -> String {
        let mut s = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        s.write_all(head(target, &format!("127.0.0.1:{port}")).as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn a_stray_request_does_not_use_up_the_listener_and_the_good_one_is_returned_once() {
        let _one = GATE.lock().await;
        let Listening { id, port } = listen().await.unwrap();
        let waiting = tokio::spawn(wait(id, "st-9"));
        until_waiting(id).await;
        assert!(
            get(port, "/callback?code=x&state=wrong")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 400"));
        let page = get(port, "/callback?code=c9&state=st-9").await;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            format!("http://127.0.0.1:{port}/callback?code=c9&state=st-9")
        );
        // Spent: the port is closed and the id is gone.
        assert!(
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
        assert_eq!(wait(id, "st-9").await.unwrap_err(), "not_listening");
    }

    #[tokio::test]
    async fn too_many_stray_requests_end_the_wait_and_close_the_port() {
        let _one = GATE.lock().await;
        let Listening { id, port } = listen().await.unwrap();
        let waiting = tokio::spawn(wait(id, "st-3"));
        until_waiting(id).await;
        // One short of the limit: still waiting, and the good redirect still gets through.
        for _ in 0..MAX_STRAY_REQUESTS - 1 {
            assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 400"));
        }
        assert!(!waiting.is_finished());
        // The last stray is answered, and then the listener is gone.
        assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 400"));
        assert_eq!(waiting.await.unwrap().unwrap_err(), "too_many_requests");
        assert!(
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
        assert_eq!(wait(id, "st-3").await.unwrap_err(), "not_listening");
        // The slot is free for the next sign-in.
        let next = listen().await.unwrap();
        cancel(next.id);
    }

    #[tokio::test]
    async fn the_good_redirect_after_many_strays_is_still_returned() {
        let _one = GATE.lock().await;
        let Listening { id, port } = listen().await.unwrap();
        let waiting = tokio::spawn(wait(id, "st-4"));
        until_waiting(id).await;
        for _ in 0..MAX_STRAY_REQUESTS - 1 {
            get(port, "/callback?code=x&state=wrong").await;
        }
        let page = get(port, "/callback?code=c4&state=st-4").await;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            format!("http://127.0.0.1:{port}/callback?code=c4&state=st-4")
        );
    }

    #[tokio::test]
    async fn cancel_and_a_newer_sign_in_stop_the_older_wait() {
        let _one = GATE.lock().await;
        let first = listen().await.unwrap();
        let waiting = tokio::spawn(wait(first.id, "a"));
        until_waiting(first.id).await;
        let second = listen().await.unwrap();
        assert_eq!(waiting.await.unwrap().unwrap_err(), "cancelled");
        let waiting = tokio::spawn(wait(second.id, "b"));
        until_waiting(second.id).await;
        cancel(second.id);
        assert_eq!(waiting.await.unwrap().unwrap_err(), "cancelled");
        // An unknown id is not "listening".
        assert_eq!(
            wait(second.id + 100, "b").await.unwrap_err(),
            "not_listening"
        );
    }
}
