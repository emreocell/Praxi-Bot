//! Gemini Live as a speech-to-text provider (see `docs/design/voice-chat.md`).
//!
//! The daemon, not the `WebView`, holds the `WebSocket` and the API key. One utterance = one short
//! session: connect, `setup`, stream the 16 kHz PCM in 100 ms chunks, `audioStreamEnd`, collect
//! `serverContent.inputTranscription`, close. The model's own answer (if the model produces one) is
//! never read: the real reply still comes from the bot model the owner chose. An utterance is capped
//! at 120 s, far below the ~10 min connection limit, so no session resumption is needed.
//!
//! The key travels only in the connection URL (as the official docs show) and that URL is never
//! logged or put into an error message.

use std::time::Duration;

use base64::Engine as _;
use daemon_api::voice::VoiceControlError;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// Production endpoint (WebSocket host).
pub(crate) const DEFAULT_WS_BASE: &str = "wss://generativelanguage.googleapis.com";
const WS_PATH: &str =
    "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
/// 100 ms of 16 kHz 16-bit mono audio.
const CHUNK_BYTES: usize = 3200;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// One audio frame (or `audioStreamEnd`) must be accepted by the socket within this time; a
/// blackholed connection would otherwise hold the request open forever.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// After `audioStreamEnd`: how long to wait for the final transcript.
const FINAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Quiet time after the last transcript piece that ends the wait.
const TRANSCRIPT_IDLE: Duration = Duration::from_millis(2500);
const SYSTEM_INSTRUCTION: &str = "You are a speech transcriber. Transcribe only, do not answer.";

/// `wss://` anywhere, `ws://` for a loopback host only (tests and local proxies). The API key is
/// appended to this base as a query parameter, so the base itself must not carry credentials, a
/// query or a fragment. The error never echoes the input.
pub(crate) fn validate_ws_base(raw: &str) -> Result<String, VoiceControlError> {
    let bad = || VoiceControlError::Internal("invalid Gemini endpoint".into());
    let url = url::Url::parse(raw).map_err(|_| bad())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "wss" => {}
        "ws" if loopback => {}
        _ => return Err(bad()),
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

/// A model that only transcribes (text out); other Live models need an audio response modality.
fn is_transcribe_model(model: &str) -> bool {
    model.contains("transcribe")
}

fn setup_message(model: &str) -> serde_json::Value {
    if is_transcribe_model(model) {
        serde_json::json!({"setup": {
            "model": format!("models/{model}"),
            "generationConfig": {"responseModalities": ["TEXT"]},
            "inputAudioTranscription": {}
        }})
    } else {
        serde_json::json!({"setup": {
            "model": format!("models/{model}"),
            "generationConfig": {"responseModalities": ["AUDIO"]},
            "systemInstruction": {"parts": [{"text": SYSTEM_INSTRUCTION}]},
            "inputAudioTranscription": {}
        }})
    }
}

pub(crate) fn upstream(msg: &str) -> VoiceControlError {
    VoiceControlError::Upstream(msg.to_owned())
}

pub(crate) fn dropped() -> VoiceControlError {
    upstream("the Gemini Live connection dropped")
}

pub(crate) fn parse(msg: Message) -> Option<serde_json::Value> {
    match msg {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        // The Live API sends its JSON in binary frames.
        Message::Binary(b) => serde_json::from_slice(&b).ok(),
        _ => None,
    }
}

pub(crate) fn closed(reason: Option<String>, key: &str) -> VoiceControlError {
    let reason = reason.unwrap_or_default().replace(key, "<key>");
    let reason: String = reason.chars().take(160).collect();
    VoiceControlError::Upstream(format!("Gemini Live closed the connection: {reason}"))
}

/// Deadlines of one session; tests shrink them.
#[derive(Clone, Copy)]
struct Limits {
    send: Duration,
    final_wait: Duration,
}

/// Send one frame within `limit`.
async fn send_within<S>(ws: &mut S, limit: Duration, msg: Message) -> Result<(), VoiceControlError>
where
    S: futures_util::Sink<Message> + Unpin,
{
    tokio::time::timeout(limit, ws.send(msg))
        .await
        .map_err(|_| upstream("sending audio to Gemini Live timed out"))?
        .map_err(|_| dropped())
}

/// Transcribe one utterance of raw 16-bit little-endian 16 kHz mono PCM.
pub(crate) async fn transcribe_pcm(
    ws_base: &str,
    key: &str,
    model: &str,
    pcm: &[u8],
) -> Result<String, VoiceControlError> {
    let limits = Limits {
        send: SEND_TIMEOUT,
        final_wait: FINAL_TIMEOUT,
    };
    transcribe_with(ws_base, key, model, pcm, limits).await
}

async fn transcribe_with(
    ws_base: &str,
    key: &str,
    model: &str,
    pcm: &[u8],
    limits: Limits,
) -> Result<String, VoiceControlError> {
    let url = format!("{ws_base}{WS_PATH}?key={key}");
    let (mut ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| upstream("the Gemini Live connection timed out"))?
        // The error is not formatted: some variants echo the request URL, which carries the key.
        .map_err(|_| upstream("could not connect to the Gemini Live API"))?;

    let frame = |v: serde_json::Value| Message::text(v.to_string());
    ws.send(frame(setup_message(model)))
        .await
        .map_err(|_| dropped())?;

    // Wait for `setupComplete` (an invalid key or model closes the socket here).
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| upstream("Gemini Live did not answer the setup"))?;
        match next {
            Some(Ok(Message::Close(f))) => {
                return Err(closed(f.map(|f| f.reason.to_string()), key));
            }
            Some(Ok(m)) => {
                if parse(m).is_some_and(|v| v.get("setupComplete").is_some()) {
                    break;
                }
            }
            _ => return Err(dropped()),
        }
    }

    let b64 = base64::engine::general_purpose::STANDARD;
    for chunk in pcm.chunks(CHUNK_BYTES) {
        let m = serde_json::json!({"realtimeInput": {"audio": {
            "data": b64.encode(chunk), "mimeType": "audio/pcm;rate=16000"}}});
        send_within(&mut ws, limits.send, frame(m)).await?;
    }
    send_within(
        &mut ws,
        limits.send,
        frame(serde_json::json!({"realtimeInput": {"audioStreamEnd": true}})),
    )
    .await?;

    let mut text = String::new();
    // Whether the service said anything usable after the audio: an empty transcript is only
    // "silence" when it did (e.g. `turnComplete`), not when the wait simply ran out.
    let mut heard_back = false;
    let hard_deadline = tokio::time::Instant::now() + limits.final_wait;
    let mut deadline = hard_deadline;
    loop {
        let Ok(next) = tokio::time::timeout_at(deadline, ws.next()).await else {
            break; // use what arrived
        };
        let msg = match next {
            Some(Ok(Message::Close(f))) => {
                if text.is_empty() {
                    return Err(closed(f.map(|f| f.reason.to_string()), key));
                }
                break;
            }
            Some(Ok(m)) => m,
            Some(Err(_)) | None => {
                if text.is_empty() {
                    return Err(dropped());
                }
                break;
            }
        };
        let Some(v) = parse(msg) else { continue };
        let Some(sc) = v.get("serverContent") else {
            continue;
        };
        heard_back = true;
        if let Some(t) = sc
            .pointer("/inputTranscription/text")
            .and_then(serde_json::Value::as_str)
        {
            text.push_str(t);
            // The transcribe model never sends `turnComplete`: once text arrived, a short quiet
            // period after the last piece ends the wait.
            deadline = hard_deadline.min(tokio::time::Instant::now() + TRANSCRIPT_IDLE);
        }
        if sc.get("turnComplete").and_then(serde_json::Value::as_bool) == Some(true) {
            break;
        }
    }
    let _ = ws.close(None).await;
    let text = text.trim().to_owned();
    if text.is_empty() && !heard_back {
        return Err(upstream(
            "Gemini Live sent no transcript before the timeout",
        ));
    }
    Ok(text)
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;

    type Seen = (String, serde_json::Value, usize, bool);

    /// A fake Live server: records the request path, setup and audio, then answers.
    async fn fake(reply_transcript: bool) -> (String, tokio::task::JoinHandle<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("ws://{}", listener.local_addr().unwrap());
        let h = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut path = String::new();
            let mut ws = accept_hdr_async(sock, |req: &Request, resp: Response| {
                path = req.uri().to_string();
                Ok(resp)
            })
            .await
            .unwrap();
            let mut setup = serde_json::Value::Null;
            let mut audio_bytes = 0;
            let mut ended = false;
            while let Some(Ok(m)) = ws.next().await {
                let Some(v) = parse(m) else { continue };
                if v.get("setup").is_some() {
                    setup = v;
                    ws.send(Message::text(r#"{"setupComplete":{}}"#))
                        .await
                        .unwrap();
                } else if let Some(a) = v.pointer("/realtimeInput/audio/data") {
                    audio_bytes += base64::engine::general_purpose::STANDARD
                        .decode(a.as_str().unwrap())
                        .unwrap()
                        .len();
                } else if v.pointer("/realtimeInput/audioStreamEnd").is_some() {
                    ended = true;
                    if reply_transcript {
                        // Binary frame, like the real service; the model's own answer must be ignored.
                        let answer = r#"{"serverContent":{"modelTurn":{"parts":[{"text":"I will answer"}]}}}"#;
                        ws.send(Message::binary(answer)).await.unwrap();
                        for t in ["merhaba ", "d\u{fc}nya"] {
                            let m = format!(
                                r#"{{"serverContent":{{"inputTranscription":{{"text":"{t}"}}}}}}"#
                            );
                            ws.send(Message::text(m)).await.unwrap();
                        }
                        ws.send(Message::text(r#"{"serverContent":{"turnComplete":true}}"#))
                            .await
                            .unwrap();
                    } else {
                        let _ = ws
                            .close(Some(CloseFrame {
                                code: 1007.into(),
                                reason: "API key SECRETKEY is not valid".into(),
                            }))
                            .await;
                    }
                }
            }
            (path, setup, audio_bytes, ended)
        });
        (base, h)
    }

    #[tokio::test]
    async fn streams_pcm_and_returns_only_the_input_transcription() {
        let (base, server) = fake(true).await;
        let pcm = vec![1u8; CHUNK_BYTES * 2 + 100];
        let text = transcribe_pcm(&base, "SECRETKEY", "gemini-3.5-transcribe-live", &pcm)
            .await
            .unwrap();
        assert_eq!(text, "merhaba d\u{fc}nya");
        let (path, setup, bytes, ended) = server.await.unwrap();
        assert!(path.contains("BidiGenerateContent?key=SECRETKEY"));
        assert_eq!(setup["setup"]["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(
            setup["setup"]["generationConfig"]["responseModalities"][0],
            "TEXT"
        );
        assert!(setup["setup"]["inputAudioTranscription"].is_object());
        assert_eq!(bytes, pcm.len());
        assert!(ended);
    }

    /// A server that accepts the session but then reads nothing or says nothing.
    async fn stalled(read_audio: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = accept_hdr_async(sock, |_: &Request, resp: Response| Ok(resp))
                .await
                .unwrap();
            while let Some(Ok(m)) = ws.next().await {
                if parse(m).is_some_and(|v| v.get("setup").is_some()) {
                    ws.send(Message::text(r#"{"setupComplete":{}}"#))
                        .await
                        .unwrap();
                    break;
                }
            }
            if read_audio {
                while ws.next().await.is_some() {}
            } else {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        base
    }

    #[tokio::test]
    async fn no_transcript_before_the_timeout_is_an_upstream_failure_not_silence() {
        let base = stalled(true).await;
        let limits = Limits {
            send: Duration::from_secs(5),
            final_wait: Duration::from_millis(300),
        };
        let err = transcribe_with(&base, "K", "m", &[0u8; 640], limits)
            .await
            .unwrap_err();
        assert!(matches!(err, VoiceControlError::Upstream(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_socket_that_stops_reading_cannot_hold_the_request_open() {
        let base = stalled(false).await;
        let limits = Limits {
            send: Duration::from_millis(300),
            final_wait: Duration::from_secs(30),
        };
        // Far more than the kernel socket buffers hold.
        let pcm = vec![7u8; 48 * 1024 * 1024];
        let started = std::time::Instant::now();
        let err = transcribe_with(&base, "K", "m", &pcm, limits)
            .await
            .unwrap_err();
        assert!(matches!(err, VoiceControlError::Upstream(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn conversational_models_get_transcribe_only_instruction() {
        let setup = setup_message("gemini-3.8-live");
        assert_eq!(
            setup["setup"]["generationConfig"]["responseModalities"][0],
            "AUDIO"
        );
        let text = setup["setup"]["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("do not answer"));
    }

    #[tokio::test]
    async fn errors_never_carry_the_key() {
        let (base, _server) = fake(false).await;
        let err = transcribe_pcm(
            &base,
            "SECRETKEY",
            "gemini-3.5-transcribe-live",
            &[0u8; 640],
        )
        .await
        .unwrap_err();
        let VoiceControlError::Upstream(m) = err else {
            panic!("{err:?}")
        };
        assert!(!m.contains("SECRETKEY"), "{m}");
        // Nothing is listening: the connect error must not echo the URL either.
        let err = transcribe_pcm("ws://127.0.0.1:1", "SECRETKEY", "m", &[0u8; 64])
            .await
            .unwrap_err();
        assert!(!format!("{err:?}").contains("SECRETKEY"));
    }

    #[test]
    fn ws_base_rules() {
        assert!(validate_ws_base(DEFAULT_WS_BASE).is_ok());
        assert!(validate_ws_base("ws://127.0.0.1:9").is_ok());
        assert!(validate_ws_base("ws://example.com").is_err());
        assert!(validate_ws_base("https://example.com").is_err());
        // The key is appended as `?key=`: a base with its own query or credentials is refused.
        assert!(validate_ws_base("wss://example.com/?a=b").is_err());
        assert!(validate_ws_base("wss://user:pw@example.com").is_err());
        assert!(validate_ws_base("wss://example.com/#x").is_err());
        // A host that merely starts with a loopback name is not loopback.
        assert!(validate_ws_base("ws://localhost.example.com").is_err());
        assert!(validate_ws_base("ws://127.0.0.1.example.com").is_err());
    }
}
