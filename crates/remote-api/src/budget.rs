//! Partitioned stream budget: long-lived bodies cannot starve control traffic.
//!
//! Every request holds one *total* permit. A long-lived response (SSE or a
//! live-view frame stream) also holds one *long* permit, and a live-view frame
//! stream one *view* permit. The long and view pools are smaller than the total,
//! so the difference is capacity that only ordinary requests (heartbeat, input,
//! release, cancel, health) can use. Acquisition never waits: overload fails at
//! once instead of queueing behind a body that may never end.
use axum::http::{HeaderMap, Method, Uri, header};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Media type of the binary live-view stream (ADR 0029 §2).
pub(crate) const VIEW_MEDIA_TYPE: &str = "application/vnd.praxi.live-view.v1";
const SSE_MEDIA_TYPE: &str = "text/event-stream";

/// What a request needs from the budget. Ordered by cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Class {
    /// Bounded exchange: JSON, health, heartbeat, input, release, cancel.
    Ordinary,
    /// Long-lived text stream (SSE).
    Stream,
    /// Long-lived binary live-view frame stream.
    View,
}

/// Capacity of one budget (a proxy, one peer connection, or a whole server).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capacity {
    pub total: usize,
    pub long: usize,
    pub view: usize,
}

/// The requested class does not fit right now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Overloaded(pub Class);

#[derive(Clone)]
pub(crate) struct Budget {
    total: Arc<Semaphore>,
    long: Arc<Semaphore>,
    view: Arc<Semaphore>,
}

impl Budget {
    pub(crate) fn new(capacity: Capacity) -> Self {
        Self {
            total: Arc::new(Semaphore::new(capacity.total)),
            long: Arc::new(Semaphore::new(capacity.long.min(capacity.total))),
            view: Arc::new(Semaphore::new(capacity.view.min(capacity.long))),
        }
    }

    /// Take permits for `class` without waiting.
    pub(crate) fn acquire(&self, class: Class) -> Result<Held, Overloaded> {
        let total = self
            .total
            .clone()
            .try_acquire_owned()
            .map_err(|_| Overloaded(Class::Ordinary))?;
        let mut held = Held {
            class: Class::Ordinary,
            _total: total,
            long: None,
            view: None,
        };
        self.upgrade(&mut held, class)?;
        Ok(held)
    }

    /// Raise already-held permits to a costlier class. Atomic: on failure the
    /// permits are exactly what they were.
    pub(crate) fn upgrade(&self, held: &mut Held, class: Class) -> Result<(), Overloaded> {
        let take = |pool: &Arc<Semaphore>| pool.clone().try_acquire_owned();
        let long = if class >= Class::Stream && held.long.is_none() {
            Some(take(&self.long).map_err(|_| Overloaded(class))?)
        } else {
            None
        };
        let view = if class >= Class::View && held.view.is_none() {
            Some(take(&self.view).map_err(|_| Overloaded(class))?)
        } else {
            None
        };
        if long.is_some() {
            held.long = long;
        }
        if view.is_some() {
            held.view = view;
        }
        held.class = held.class.max(class);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn available(&self) -> (usize, usize, usize) {
        (
            self.total.available_permits(),
            self.long.available_permits(),
            self.view.available_permits(),
        )
    }
}

/// Permits for one request; dropping them returns every unit.
pub(crate) struct Held {
    pub(crate) class: Class,
    _total: OwnedSemaphorePermit,
    long: Option<OwnedSemaphorePermit>,
    view: Option<OwnedSemaphorePermit>,
}

/// Percent-decode, collapse repeated slashes and drop a trailing slash.
pub(crate) fn normalized_path(raw: &str) -> String {
    let decoded = percent_decode(raw);
    let mut out = String::with_capacity(decoded.len());
    let mut slash = false;
    for ch in decoded.chars() {
        if ch == '/' {
            if !slash {
                out.push('/');
            }
            slash = true;
        } else {
            out.push(ch);
            slash = false;
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| {
            char::from(b)
                .to_digit(16)
                .and_then(|d| u8::try_from(d).ok())
        };
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn accepts(headers: &HeaderMap, media: &str) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.to_ascii_lowercase().contains(media))
}

/// Classify from the request line and `Accept`, never from client identity.
pub(crate) fn request_class(method: &Method, uri: &Uri, headers: &HeaderMap) -> Class {
    if *method != Method::GET {
        return Class::Ordinary;
    }
    let path = normalized_path(uri.path());
    if accepts(headers, VIEW_MEDIA_TYPE)
        || (path.starts_with("/v1/view/") && path.ends_with("/frames"))
    {
        Class::View
    } else if path == "/v1/events" || accepts(headers, SSE_MEDIA_TYPE) {
        Class::Stream
    } else {
        Class::Ordinary
    }
}

/// Backstop: an unusual request spelling cannot hide a long-lived body.
pub(crate) fn response_class(headers: &HeaderMap) -> Class {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if content_type.starts_with(VIEW_MEDIA_TYPE) {
        Class::View
    } else if content_type.starts_with(SSE_MEDIA_TYPE) {
        Class::Stream
    } else {
        Class::Ordinary
    }
}

/// Request headers that could be mistaken for transport identity.
pub(crate) fn strip_identity_headers(headers: &mut HeaderMap) {
    let names: Vec<_> = headers
        .keys()
        .filter(|name| {
            let n = name.as_str();
            n.starts_with("x-praxi-")
                || n.starts_with("x-peer-")
                || n.starts_with("x-forwarded-")
                || n == "forwarded"
                || n == "x-real-ip"
        })
        .cloned()
        .collect();
    for name in names {
        headers.remove(name);
    }
}

/// Query keys that never carry credentials on view or control routes.
pub(crate) fn view_credential_query(uri: &Uri) -> bool {
    let path = normalized_path(uri.path());
    (path.starts_with("/v1/view/")
        || matches!(
            path.as_str(),
            "/v1/control/takeover" | "/v1/control/release"
        ))
        && uri.query().is_some_and(|q| {
            q.split('&').any(|kv| {
                let key = percent_decode(kv.split('=').next().unwrap_or_default());
                matches!(key.as_str(), "token" | "access_token" | "authorization")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Capacity = Capacity {
        total: 8,
        long: 5,
        view: 2,
    };

    #[test]
    fn view_and_long_streams_leave_reserve_for_ordinary_requests() {
        let budget = Budget::new(CAP);
        let views: Vec<_> = (0..2)
            .map(|_| budget.acquire(Class::View).unwrap())
            .collect();
        assert!(budget.acquire(Class::View).is_err());
        let streams: Vec<_> = (0..3)
            .map(|_| budget.acquire(Class::Stream).unwrap())
            .collect();
        assert!(budget.acquire(Class::Stream).is_err());
        // 5 long-lived bodies leave exactly total - long = 3 ordinary slots.
        let ordinary: Vec<_> = (0..3)
            .map(|_| budget.acquire(Class::Ordinary).unwrap())
            .collect();
        assert!(budget.acquire(Class::Ordinary).is_err());
        drop((views, streams));
        assert_eq!(budget.available(), (5, 5, 2));
        drop(ordinary);
        assert_eq!(budget.available(), (8, 5, 2));
    }

    #[test]
    fn failed_upgrade_keeps_ordinary_permit_and_failed_acquire_leaks_nothing() {
        let budget = Budget::new(CAP);
        let _views: Vec<_> = (0..2)
            .map(|_| budget.acquire(Class::View).unwrap())
            .collect();
        let before = budget.available();
        assert!(budget.acquire(Class::View).is_err());
        assert_eq!(budget.available(), before);
        let mut held = budget.acquire(Class::Ordinary).unwrap();
        assert!(budget.upgrade(&mut held, Class::View).is_err());
        assert_eq!(held.class, Class::Ordinary);
        drop(held);
        assert_eq!(budget.available(), before);
    }

    #[test]
    fn classification_uses_decoded_path_accept_and_response_type() {
        let get = Method::GET;
        let h = HeaderMap::new();
        let uri = |s: &str| s.parse::<Uri>().unwrap();
        for path in [
            "/v1/view/sessions/abc/frames",
            "/v1/view//sessions/abc/frames/",
            "/v1/view/sessions/abc/%66rames",
            "/v1/%76iew/sessions/abc/frames",
        ] {
            assert_eq!(request_class(&get, &uri(path), &h), Class::View, "{path}");
        }
        assert_eq!(
            request_class(&Method::POST, &uri("/v1/view/sessions/abc/frames"), &h),
            Class::Ordinary
        );
        assert_eq!(request_class(&get, &uri("/v1/events"), &h), Class::Stream);
        assert_eq!(
            request_class(&get, &uri("/v1/view/targets"), &h),
            Class::Ordinary
        );
        let mut accept = HeaderMap::new();
        accept.insert(header::ACCEPT, VIEW_MEDIA_TYPE.parse().unwrap());
        assert_eq!(request_class(&get, &uri("/anything"), &accept), Class::View);
        let mut sse = HeaderMap::new();
        sse.insert(
            header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8".parse().unwrap(),
        );
        assert_eq!(response_class(&sse), Class::Stream);
        let mut view = HeaderMap::new();
        view.insert(header::CONTENT_TYPE, VIEW_MEDIA_TYPE.parse().unwrap());
        assert_eq!(response_class(&view), Class::View);
        assert_eq!(response_class(&h), Class::Ordinary);
    }

    #[test]
    fn credential_query_on_view_routes_is_detected_even_when_encoded() {
        for uri in [
            "/v1/view/targets?token=x",
            "/v1/view/sessions/a/frames?%74oken=x",
            "/v1/view/sessions/a/frames?access_token=x",
            "/v1/control/takeover?authorization=x",
            "/v1/%76iew/targets?token=x",
        ] {
            assert!(view_credential_query(&uri.parse().unwrap()), "{uri}");
        }
        for uri in [
            "/v1/view/targets?keep=1",
            "/v1/events?token=x",
            "/v1/bots?token=x",
        ] {
            assert!(!view_credential_query(&uri.parse().unwrap()), "{uri}");
        }
    }
}
