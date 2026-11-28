//! A fake `AgentMail` server: the real connector talks to it over loopback HTTP, the fake sites
//! drop their mails into it, and nothing else ever arrives.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The bot's own inbox on the fake provider.
pub struct FakeAgentMail {
    server: MockServer,
    inbox: String,
    messages: Arc<Mutex<Vec<Value>>>,
    /// `METHOD path` of every request the provider saw (to prove nothing was ever sent).
    seen: Arc<Mutex<Vec<String>>>,
}

/// RFC 3339 UTC for `secs` since the epoch.
fn rfc3339(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap();
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl FakeAgentMail {
    pub async fn start(inbox: &str) -> Self {
        let server = MockServer::start().await;
        let messages: Arc<Mutex<Vec<Value>>> = Arc::default();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let enc = inbox.replace('@', "%40");
        let list = Arc::clone(&messages);
        let log = Arc::clone(&seen);
        Mock::given(method("GET"))
            .and(path_regex(format!(
                "^/v0/inboxes/{}/messages$",
                regex_escape(&enc)
            )))
            .respond_with(move |req: &wiremock::Request| {
                log.lock().push(format!("GET {}", req.url.path()));
                let all = list.lock().clone();
                ResponseTemplate::new(200)
                    .set_body_json(json!({"count": all.len(), "messages": all}))
            })
            .mount(&server)
            .await;
        let one = Arc::clone(&messages);
        let log = Arc::clone(&seen);
        Mock::given(method("GET"))
            .and(path_regex(format!(
                "^/v0/inboxes/{}/messages/[^/]+$",
                regex_escape(&enc)
            )))
            .respond_with(move |req: &wiremock::Request| {
                log.lock().push(format!("GET {}", req.url.path()));
                let id = req
                    .url
                    .path()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let id = url::form_urlencoded::parse(format!("x={id}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or(id);
                match one.lock().iter().find(|m| m["message_id"] == id) {
                    Some(m) => ResponseTemplate::new(200).set_body_json(m.clone()),
                    None => {
                        ResponseTemplate::new(404).set_body_json(json!({"message": "not found"}))
                    }
                }
            })
            .mount(&server)
            .await;
        // Anything else (a send, a delete, a label change) is recorded and refused: a bot that
        // only signs up must never write to its provider.
        let log = Arc::clone(&seen);
        Mock::given(wiremock::matchers::any())
            .respond_with(move |req: &wiremock::Request| {
                log.lock()
                    .push(format!("{} {}", req.method, req.url.path()));
                ResponseTemplate::new(405)
            })
            .with_priority(10)
            .mount(&server)
            .await;
        Self {
            server,
            inbox: inbox.to_owned(),
            messages,
            seen,
        }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub fn address(&self) -> &str {
        &self.inbox
    }

    /// A mail arrives now.
    pub fn deliver(&self, from: &str, subject: &str, text: &str, html: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = rfc3339(now);
        let n = self.messages.lock().len() + 1;
        self.messages.lock().push(json!({
            "inbox_id": self.inbox, "thread_id": format!("t{n}"), "message_id": format!("msg{n}"),
            "labels": ["unread"], "timestamp": ts, "from": from, "to": [self.inbox],
            "size": text.len() + html.len(), "updated_at": ts, "created_at": ts,
            "subject": subject, "text": text, "html": html, "attachments": []
        }));
    }

    /// Requests that were not a read of the inbox.
    pub fn writes(&self) -> Vec<String> {
        self.seen
            .lock()
            .iter()
            .filter(|r| !r.starts_with("GET "))
            .cloned()
            .collect()
    }
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || c == '%' {
                vec![c]
            } else {
                vec!['\\', c]
            }
        })
        .collect()
}
