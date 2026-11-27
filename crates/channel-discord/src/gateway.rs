//! The Discord gateway (a websocket): receives messages and button presses.
//!
//! Every event is turned into the shared [`IncomingMessage`] here, from the
//! service's own authenticated fields. Nothing in message text decides who
//! the sender is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use channel_api::{ChatRef, IncomingKind, IncomingMessage, SenderIdentity};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use secret_store::SecretValue;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::text::attachment_label;

/// The real gateway address.
pub const DEFAULT_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// `GUILD_MESSAGES` | `DIRECT_MESSAGES` | `MESSAGE_CONTENT`.
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);
const MAX_RECONNECT_WAIT: Duration = Duration::from_secs(30);

/// Chat reference of a direct message channel.
#[must_use]
pub fn dm_chat(channel_id: &str) -> ChatRef {
    ChatRef(format!("dm:{channel_id}"))
}

/// Chat reference of a server channel.
#[must_use]
pub fn guild_chat(guild_id: &str, channel_id: &str) -> ChatRef {
    ChatRef(format!("guild:{guild_id}:{channel_id}"))
}

/// A parsed chat reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatParts<'a> {
    /// Server id, `None` for a direct message.
    pub guild_id: Option<&'a str>,
    /// Channel id.
    pub channel_id: &'a str,
}

/// Split `dm:<channel>` or `guild:<guild>:<channel>`.
#[must_use]
pub fn parse_chat(chat: &ChatRef) -> Option<ChatParts<'_>> {
    let s = chat.0.as_str();
    if let Some(c) = s.strip_prefix("dm:") {
        return Some(ChatParts {
            guild_id: None,
            channel_id: c,
        });
    }
    let rest = s.strip_prefix("guild:")?;
    let (g, c) = rest.split_once(':')?;
    Some(ChatParts {
        guild_id: Some(g),
        channel_id: c,
    })
}

fn chat_for(d: &Value) -> Option<ChatRef> {
    let channel = d["channel_id"].as_str()?;
    Some(match d["guild_id"].as_str() {
        Some(g) => guild_chat(g, channel),
        None => dm_chat(channel),
    })
}

/// Turn a gateway `MESSAGE_CREATE` into an event. Bots (including this one), system messages
/// and forwarded messages are dropped.
#[must_use]
pub fn message_event(d: &Value) -> Option<IncomingMessage> {
    let author = &d["author"];
    if author["bot"].as_bool().unwrap_or(false) || d["webhook_id"].is_string() {
        return None;
    }
    // 0 = normal message, 19 = reply; everything else is a system notice.
    if !matches!(d["type"].as_u64().unwrap_or(0), 0 | 19) {
        return None;
    }
    // A forward (`message_reference.type` 1, with `message_snapshots`) carries someone else's
    // words under the forwarder's name: it is never read as the sender's own text or command.
    if d["message_reference"]["type"].as_u64() == Some(1)
        || d["message_snapshots"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    {
        return None;
    }
    let id = d["id"].as_str()?;
    let sender = SenderIdentity {
        id: author["id"].as_str()?.to_owned(),
        display_name: author["global_name"]
            .as_str()
            .or_else(|| author["username"].as_str())
            .map(|s| s.chars().take(40).collect()),
    };
    let content = d["content"].as_str().unwrap_or("").trim();
    let labels: Vec<String> = d["attachments"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(10)
                .map(|f| {
                    attachment_label(
                        f["filename"].as_str().unwrap_or("file"),
                        f["content_type"].as_str(),
                        f["size"].as_u64(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let kind = match (content.is_empty(), labels.is_empty()) {
        (true, true) => return None,
        (true, false) => IncomingKind::Unsupported(labels.join(" ")),
        (false, true) => IncomingKind::Text(content.to_owned()),
        (false, false) => IncomingKind::Text(format!("{content}\n{}", labels.join("\n"))),
    };
    Some(IncomingMessage {
        event_id: format!("msg:{id}"),
        chat: chat_for(d)?,
        sender,
        message_id: Some(id.to_owned()),
        kind,
    })
}

/// Turn a gateway `INTERACTION_CREATE` (a pressed button) into a callback event.
#[must_use]
pub fn interaction_event(d: &Value) -> Option<IncomingMessage> {
    // 3 = message component (a button press).
    if d["type"].as_u64()? != 3 {
        return None;
    }
    let id = d["id"].as_str()?;
    let token = d["token"].as_str()?;
    let user = if d["member"]["user"].is_object() {
        &d["member"]["user"]
    } else {
        &d["user"]
    };
    Some(IncomingMessage {
        event_id: format!("int:{id}"),
        chat: chat_for(d)?,
        sender: SenderIdentity {
            id: user["id"].as_str()?.to_owned(),
            display_name: None,
        },
        message_id: d["message"]["id"].as_str().map(str::to_owned),
        kind: IncomingKind::Callback {
            query_id: format!("{id}:{token}"),
            data: d["data"]["custom_id"].as_str()?.to_owned(),
            message_id: d["message"]["id"].as_str().map(str::to_owned),
        },
    })
}

/// What the gateway task tells the rest of the adapter.
#[derive(Debug, Default)]
pub struct GatewayHealth {
    /// A session is open and READY was received.
    pub connected: AtomicBool,
    /// The last reason the gateway had to give up or reconnect.
    pub last_error: Mutex<Option<String>>,
    /// The gateway refused us for good; do not retry.
    pub fatal: AtomicBool,
}

enum Session {
    /// Reconnect; `ready` tells whether this session got as far as READY.
    Reconnect {
        ready: bool,
    },
    Fatal(String),
    Stopped,
}

#[derive(Default)]
struct Resume {
    session_id: Option<String>,
    seq: Option<u64>,
    url: Option<String>,
}

fn close_reason(code: u16) -> Option<&'static str> {
    match code {
        4004 => {
            Some("Discord rejected the bot token. Paste a fresh token from the Developer Portal.")
        }
        4010..=4012 => Some("Discord refused this connection setup."),
        4013 => Some("Discord rejected the requested permissions (intents)."),
        4014 => Some(
            "The Message Content switch is off for this bot. Turn it on in the Discord Developer \
             Portal under Bot, Privileged Gateway Intents.",
        ),
        _ => None,
    }
}

/// Run the gateway until `cancel` fires or Discord refuses the bot for good.
/// Reconnects with a growing wait. Events go to `tx`; if the receiver is gone the
/// task ends.
pub async fn run(
    url: String,
    token: SecretValue,
    base_backoff: Duration,
    tx: mpsc::Sender<IncomingMessage>,
    cancel: CancellationToken,
    health: std::sync::Arc<GatewayHealth>,
) {
    let mut wait = base_backoff;
    let mut resume = Resume::default();
    loop {
        let outcome = session(&url, &token, &tx, &cancel, &health, &mut resume).await;
        health.connected.store(false, Ordering::SeqCst);
        match outcome {
            Session::Stopped => return,
            Session::Fatal(reason) => {
                *health.last_error.lock() = Some(reason);
                health.fatal.store(true, Ordering::SeqCst);
                return;
            }
            Session::Reconnect { ready } => {
                if ready {
                    wait = base_backoff;
                }
            }
        }
        if tx.is_closed() {
            return;
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        wait = (wait * 2).min(MAX_RECONNECT_WAIT);
    }
}

#[allow(clippy::too_many_lines)]
async fn session(
    url: &str,
    token: &SecretValue,
    tx: &mpsc::Sender<IncomingMessage>,
    cancel: &CancellationToken,
    health: &GatewayHealth,
    resume: &mut Resume,
) -> Session {
    let connection = tokio::select! {
        () = cancel.cancelled() => return Session::Stopped,
        r = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(resume.url.as_deref().unwrap_or(url))) => r,
    };
    let Ok(Ok((mut ws, _))) = connection else {
        *health.last_error.lock() = Some("cannot reach the Discord gateway".into());
        return Session::Reconnect { ready: false };
    };
    let mut hb: Option<tokio::time::Interval> = None;
    let mut acked = true;
    let mut ready = false;
    loop {
        let tick = async {
            match hb.as_mut() {
                Some(i) => {
                    i.tick().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(None)).await;
                return Session::Stopped;
            }
            () = tick => {
                if !acked {
                    return Session::Reconnect { ready };
                }
                acked = false;
                if ws.send(Message::Text(json!({"op": 1, "d": resume.seq}).to_string().into())).await.is_err() {
                    return Session::Reconnect { ready };
                }
            }
            frame = ws.next() => {
                let Some(Ok(frame)) = frame else {
                    return Session::Reconnect { ready };
                };
                let text = match frame {
                    Message::Text(t) => t,
                    Message::Close(f) => {
                        let code = f.map_or(0, |f| u16::from(f.code));
                        if matches!(code, 4007 | 4009) { *resume = Resume::default(); }
                        return match close_reason(code) {
                            Some(r) => Session::Fatal(r.into()),
                            None => Session::Reconnect { ready },
                        };
                    }
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                if let Some(s) = v["s"].as_u64() {
                    resume.seq = Some(s);
                }
                match v["op"].as_u64() {
                    Some(10) => {
                        let ms = v["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250).clamp(10, 120_000);
                        let mut i = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(ms / 2), Duration::from_millis(ms));
                        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        hb = Some(i);
                        let identify = if let (Some(id), Some(seq)) = (&resume.session_id, resume.seq) {
                            json!({"op": 6, "d": {"token": token.expose(), "session_id": id, "seq": seq}})
                        } else { json!({"op": 2, "d": {
                            "token": token.expose(),
                            "intents": INTENTS,
                            "properties": {"os": std::env::consts::OS, "browser": "praxi-bot", "device": "praxi-bot"},
                        }}) };
                        if ws.send(Message::Text(identify.to_string().into())).await.is_err() {
                            return Session::Reconnect { ready };
                        }
                    }
                    Some(11) => acked = true,
                    // The server asks for a heartbeat now.
                    Some(1) => {
                        let _ = ws.send(Message::Text(json!({"op": 1, "d": resume.seq}).to_string().into())).await;
                    }
                    Some(7) => return Session::Reconnect { ready },
                    Some(9) => {
                        if v["d"] != true { *resume = Resume::default(); }
                        return Session::Reconnect { ready };
                    }
                    Some(0) => {
                        let d = &v["d"];
                        let event = match v["t"].as_str() {
                            Some("READY") => {
                                resume.session_id = d["session_id"].as_str().map(str::to_owned);
                                // Resume addresses are service-authored, but must still
                                // be Discord TLS endpoints before receiving our token.
                                resume.url = d["resume_gateway_url"].as_str().and_then(|s| {
                                    let mut u = reqwest::Url::parse(s).ok()?;
                                    let host = u.host_str()?;
                                    if u.scheme() != "wss" || !(host == "gateway.discord.gg" || host.ends_with(".discord.gg")) {
                                        return None;
                                    }
                                    u.set_query(Some("v=10&encoding=json"));
                                    Some(u.to_string())
                                });
                                ready = true;
                                health.connected.store(true, Ordering::SeqCst);
                                *health.last_error.lock() = None;
                                None
                            }
                            Some("RESUMED") => {
                                ready = true;
                                health.connected.store(true, Ordering::SeqCst);
                                *health.last_error.lock() = None;
                                None
                            }
                            Some("MESSAGE_CREATE") => message_event(d),
                            Some("INTERACTION_CREATE") => interaction_event(d),
                            _ => None,
                        };
                        if let Some(event) = event && tx.send(event).await.is_err() {
                            return Session::Stopped;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
