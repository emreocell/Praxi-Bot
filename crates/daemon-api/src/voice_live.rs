//! Live voice conversation (`/v1/voice/live*`, `docs/design/voice-live.md`).
//!
//! The embedder (`apps/cli/src/voice_live.rs`) holds the provider `WebSocket` and the API key; this
//! module is the provider-neutral half: one owner-only session at a time, its event stream (SSE
//! over the response of the opening `POST`), the audio and tool-result uploads, and the route that
//! writes the finished conversation into a chat as text messages.
//!
//! The session has **no tools of its own**. The one function the voice model may call (`ask_bot`)
//! is handed to the app as a `tool_call` event; the app sends the request as an ordinary chat
//! message and returns the bot's final text, so every action goes through the normal run, the
//! permission engine and the on-screen approval cards. Nothing here approves or denies anything.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use base64::Engine as _;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use storage::{Message, now_ms};
use tokio::sync::mpsc;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::routes::{ApiJson, id};
use crate::voice::VoiceControlError;
use protocol::ChatId;

/// Microphone audio the app sends: 16-bit little-endian PCM, mono.
pub const INPUT_RATE: u32 = 16_000;
/// Audio the voice model sends back: 16-bit little-endian PCM, mono.
pub const OUTPUT_RATE: u32 = 24_000;
/// Largest audio upload: one second of input.
const MAX_AUDIO_CHUNK: usize = INPUT_RATE as usize * 2;
/// Longest accepted tool result (characters).
const MAX_TOOL_TEXT: usize = 8_000;
/// Transcript limits of one write.
const MAX_TURNS: usize = 400;
const MAX_TURN_CHARS: usize = 8_000;

/// What the app sends to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveUp {
    /// 16 kHz mono PCM16 from the microphone.
    Audio(Vec<u8>),
    /// The bot's answer to a `tool_call` (or an error text).
    ToolResult {
        /// Id from the `tool_call` event.
        call_id: String,
        /// What the bot said or why it could not.
        text: String,
    },
    /// An update the voice model should tell the owner about (the bot finished after a handover).
    Note(String),
}

/// What the session tells the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveDown {
    /// 24 kHz mono PCM16 of the voice model's speech.
    Audio(Vec<u8>),
    /// A piece of what the owner said (text, as heard).
    User(String),
    /// A piece of what the voice model says.
    Bot(String),
    /// The voice model finished its turn.
    TurnComplete,
    /// The owner spoke over the voice model: drop queued audio.
    Interrupted,
    /// The voice model asks the bot to do something.
    ToolCall {
        /// Id to answer with.
        call_id: String,
        /// The self-contained request.
        request: String,
    },
    /// Calls the voice model no longer waits for.
    ToolCancelled(Vec<String>),
    /// The connection is being renewed (the provider limits one connection to about 10 minutes).
    Reconnecting,
    /// The session is over.
    Ended {
        /// `limit`, `idle`, `error` or `closed`.
        reason: &'static str,
        /// Plain sentence for `error`.
        message: String,
    },
}

/// What a session needs to start.
#[derive(Debug, Clone, Default)]
pub struct LiveOpen {
    /// UI language (`en`, `tr`, ...), a hint for the voice model.
    pub language: Option<String>,
    /// The bot's name, so the voice model speaks as it.
    pub assistant_name: Option<String>,
}

/// A running session as the embedder returns it (dropping `up` ends it).
pub struct LiveLink {
    /// Provider model id, shown to the owner.
    pub model: String,
    /// Longest the session lasts, in seconds.
    pub max_seconds: u32,
    /// App to session.
    pub up: mpsc::Sender<LiveUp>,
    /// Session to app.
    pub down: mpsc::Receiver<LiveDown>,
}

/// The one active session (`None` slot = free).
pub(crate) struct LiveSlot {
    id: Uuid,
    up: Option<mpsc::Sender<LiveUp>>,
}

/// Body of `POST /v1/voice/live`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct LiveOpenInput {
    /// UI language hint (ISO-639-1).
    #[serde(default)]
    pub language: Option<String>,
    /// Name of the bot the owner is talking to.
    #[serde(default)]
    pub assistant_name: Option<String>,
}

/// Body of `POST /v1/voice/live/{id}/tool`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LiveToolResult {
    /// `call_id` of the `tool_call` event.
    pub call_id: String,
    /// The bot's final text, or why it could not do it.
    pub text: String,
}

/// Body of `POST /v1/voice/live/{id}/note`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LiveNote {
    /// Plain text the voice model passes on.
    pub text: String,
}

/// One line of a finished conversation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct VoiceTurn {
    /// `user` (the owner) or `assistant` (the voice model).
    pub role: String,
    /// What was said.
    pub text: String,
}

/// Body of `POST /v1/chats/{id}/voice-transcript`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct VoiceTranscriptInput {
    /// Lines in order.
    pub turns: Vec<VoiceTurn>,
}

/// Result of `POST /v1/chats/{id}/voice-transcript`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VoiceTranscriptOutput {
    /// Messages written.
    pub saved: u32,
}

/// One server-sent event as JSON. Audio travels base64 inside the event.
pub(crate) fn event_json(down: &LiveDown) -> serde_json::Value {
    use serde_json::json;
    match down {
        LiveDown::Audio(pcm) => json!({
            "type": "audio",
            "data": base64::engine::general_purpose::STANDARD.encode(pcm),
        }),
        LiveDown::User(t) => json!({"type": "you", "text": t}),
        LiveDown::Bot(t) => json!({"type": "bot", "text": t}),
        LiveDown::TurnComplete => json!({"type": "turn_complete"}),
        LiveDown::Interrupted => json!({"type": "interrupted"}),
        LiveDown::ToolCall { call_id, request } => {
            json!({"type": "tool_call", "call_id": call_id, "request": request})
        }
        LiveDown::ToolCancelled(ids) => json!({"type": "tool_cancelled", "call_ids": ids}),
        LiveDown::Reconnecting => json!({"type": "reconnecting"}),
        LiveDown::Ended { reason, message } => {
            json!({"type": "ended", "reason": reason, "message": message})
        }
    }
}

/// Frees the slot when the event stream is dropped (the app closed the response or the daemon
/// stops); dropping the sender then ends the provider connection.
struct SlotGuard {
    state: AppState,
    id: Uuid,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut slot = self.state.0.live_voice.lock();
        if slot.as_ref().is_some_and(|s| s.id == self.id) {
            *slot = None;
        }
    }
}

fn voice(s: &AppState) -> Result<Arc<dyn crate::voice::VoiceControl>, ApiError> {
    s.voice()
        .ok_or_else(|| ApiError::unavailable("voice is not available in this daemon"))
}

fn live_error(e: VoiceControlError) -> ApiError {
    match e {
        // The dictation wording ("add a transcription service") would point at the wrong setting.
        VoiceControlError::NotConfigured => ApiError::new(
            StatusCode::CONFLICT,
            "live_voice_not_configured",
            "live voice needs a Gemini API key: add one in Settings",
        ),
        other => ApiError::from(other),
    }
}

fn session_up(s: &AppState, session: &str) -> Result<mpsc::Sender<LiveUp>, ApiError> {
    let session = id(session)?;
    s.0.live_voice
        .lock()
        .as_ref()
        .filter(|slot| slot.id == session)
        .and_then(|slot| slot.up.clone())
        .ok_or_else(|| ApiError::not_found("live voice session"))
}

/// Open a live voice conversation. The response is an event stream (`data:` lines of JSON, one
/// `type` field each): `ready`, then `audio`, `you`, `bot`, `turn_complete`, `interrupted`,
/// `tool_call`, `tool_cancelled`, `reconnecting` and finally `ended`. Closing the response ends
/// the session. Owner only; one session at a time.
#[utoipa::path(post, path = "/v1/voice/live", tag = "voice", security(("bearer" = [])),
    request_body = LiveOpenInput,
    responses((status = 200, description = "Server-sent events of the conversation", content_type = "text/event-stream"),
              (status = 409, body = ErrorBody, description = "no Gemini key, or another conversation is open"),
              (status = 422, body = ErrorBody, description = "invalid live voice settings"),
              (status = 502, body = ErrorBody, description = "the voice service failed"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn open(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<LiveOpenInput>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let control = voice(&s)?;
    let session = Uuid::now_v7();
    {
        let mut slot = s.0.live_voice.lock();
        if slot.is_some() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "conflict",
                "a live voice conversation is already open",
            ));
        }
        *slot = Some(LiveSlot {
            id: session,
            up: None,
        });
    }
    let guard = SlotGuard {
        state: s.clone(),
        id: session,
    };
    let clean = |v: Option<String>, max: usize| {
        v.map(|v| v.trim().chars().take(max).collect::<String>())
            .filter(|v| !v.is_empty())
    };
    let open = LiveOpen {
        language: clean(input.language, 12)
            .filter(|l| l.chars().all(|c| c.is_ascii_alphabetic() || c == '-')),
        assistant_name: clean(input.assistant_name, 80),
    };
    let link = control.live_open(open).await.map_err(live_error)?;
    if let Some(slot) = s.0.live_voice.lock().as_mut() {
        slot.up = Some(link.up.clone());
    }
    let ready = serde_json::json!({
        "type": "ready",
        "session_id": session,
        "model": link.model,
        "input_rate": INPUT_RATE,
        "output_rate": OUTPUT_RATE,
        "max_seconds": link.max_seconds,
    });
    // `up` stays in the slot (for the upload routes); the stream owns the receiver.
    let state = (Some(ready), link.down, guard, false);
    Ok(Sse::new(futures::stream::unfold(
        state,
        |(first, mut down, guard, done)| async move {
            if done {
                return None;
            }
            if let Some(ready) = first {
                let ev = SseEvent::default().data(ready.to_string());
                return Some((Ok(ev), (None, down, guard, false)));
            }
            let (json, done) = match down.recv().await {
                Some(d) => {
                    let end = matches!(d, LiveDown::Ended { .. });
                    (event_json(&d), end)
                }
                None => (
                    event_json(&LiveDown::Ended {
                        reason: "closed",
                        message: String::new(),
                    }),
                    true,
                ),
            };
            let ev = SseEvent::default().data(json.to_string());
            Some((Ok(ev), (None, down, guard, done)))
        },
    ))
    .keep_alive(KeepAlive::default()))
}

/// Upload microphone audio (raw 16-bit little-endian PCM, 16 kHz, mono; up to one second).
#[utoipa::path(post, path = "/v1/voice/live/{id}/audio", tag = "voice", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "session_id of the ready event")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 204, description = "Accepted"),
              (status = 400, body = ErrorBody, description = "bad audio"),
              (status = 404, body = ErrorBody, description = "no such conversation"),
              (status = 503, body = ErrorBody, description = "the session cannot take audio right now")))]
pub(crate) async fn audio(
    State(s): State<AppState>,
    Path(session): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if body.is_empty() || body.len() > MAX_AUDIO_CHUNK || !body.len().is_multiple_of(2) {
        return Err(ApiError::bad_request(format!(
            "audio must be 2 to {MAX_AUDIO_CHUNK} bytes of 16-bit PCM"
        )));
    }
    let up = session_up(&s, &session)?;
    up.try_send(LiveUp::Audio(body.to_vec()))
        .map_err(|_| ApiError::unavailable("the live voice session cannot take audio right now"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Return the bot's answer to a `tool_call`.
#[utoipa::path(post, path = "/v1/voice/live/{id}/tool", tag = "voice", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "session_id of the ready event")),
    request_body = LiveToolResult,
    responses((status = 204, description = "Accepted"),
              (status = 400, body = ErrorBody, description = "bad request"),
              (status = 404, body = ErrorBody, description = "no such conversation"),
              (status = 503, body = ErrorBody, description = "the session is gone")))]
pub(crate) async fn tool_result(
    State(s): State<AppState>,
    Path(session): Path<String>,
    ApiJson(input): ApiJson<LiveToolResult>,
) -> Result<StatusCode, ApiError> {
    if input.call_id.is_empty() || input.call_id.len() > 200 {
        return Err(ApiError::bad_request("`call_id` is missing or too long"));
    }
    let text: String = input.text.chars().take(MAX_TOOL_TEXT).collect();
    let up = session_up(&s, &session)?;
    up.send(LiveUp::ToolResult {
        call_id: input.call_id,
        text,
    })
    .await
    .map_err(|_| ApiError::unavailable("the live voice session has ended"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Pass an update to the voice model (the bot finished a task the owner was told to wait for).
#[utoipa::path(post, path = "/v1/voice/live/{id}/note", tag = "voice", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "session_id of the ready event")),
    request_body = LiveNote,
    responses((status = 204, description = "Accepted"),
              (status = 404, body = ErrorBody, description = "no such conversation"),
              (status = 503, body = ErrorBody, description = "the session is gone")))]
pub(crate) async fn note(
    State(s): State<AppState>,
    Path(session): Path<String>,
    ApiJson(input): ApiJson<LiveNote>,
) -> Result<StatusCode, ApiError> {
    let text: String = input.text.chars().take(MAX_TOOL_TEXT).collect();
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("`text` must not be empty"));
    }
    let up = session_up(&s, &session)?;
    up.send(LiveUp::Note(text))
        .await
        .map_err(|_| ApiError::unavailable("the live voice session has ended"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// End a conversation now.
#[utoipa::path(delete, path = "/v1/voice/live/{id}", tag = "voice", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "session_id of the ready event")),
    responses((status = 204, description = "Ended (or already gone)")))]
pub(crate) async fn close(
    State(s): State<AppState>,
    Path(session): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = id(&session)?;
    let mut slot = s.0.live_voice.lock();
    if slot.as_ref().is_some_and(|x| x.id == session) {
        // Dropping the sender ends the provider connection; the stream then reports `ended`.
        *slot = None;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Write a finished voice conversation into a chat as ordinary text messages (no run starts).
#[utoipa::path(post, path = "/v1/chats/{id}/voice-transcript", tag = "chats", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    request_body = VoiceTranscriptInput,
    responses((status = 201, body = VoiceTranscriptOutput),
              (status = 400, body = ErrorBody, description = "bad transcript"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn save_transcript(
    State(s): State<AppState>,
    Path(cid): Path<String>,
    ApiJson(input): ApiJson<VoiceTranscriptInput>,
) -> Result<(StatusCode, Json<VoiceTranscriptOutput>), ApiError> {
    let chat = s.chat_of_user(ChatId(id(&cid)?))?;
    if input.turns.len() > MAX_TURNS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_TURNS} lines per transcript"
        )));
    }
    // Check every line first: a bad one must not leave half a conversation behind.
    let mut lines = Vec::with_capacity(input.turns.len());
    for turn in &input.turns {
        let text = turn.text.trim();
        if text.is_empty() {
            continue;
        }
        if !matches!(turn.role.as_str(), "user" | "assistant") {
            return Err(ApiError::bad_request(
                "`role` must be `user` or `assistant`",
            ));
        }
        if text.chars().count() > MAX_TURN_CHARS {
            return Err(ApiError::bad_request(format!(
                "a line is longer than {MAX_TURN_CHARS} characters"
            )));
        }
        lines.push((turn.role.as_str(), text));
    }
    let base = now_ms();
    let mut saved = 0_u32;
    for (i, (role, text)) in lines.into_iter().enumerate() {
        s.storage().insert_message(&Message {
            id: Uuid::now_v7(),
            user_id: s.user_id(),
            chat_id: chat.id,
            run_id: None,
            role: role.to_owned(),
            content: text.to_owned(),
            // Keep the order of the conversation even inside one millisecond.
            created_at_ms: base + i64::try_from(i).unwrap_or(0),
            exposure_untrusted: false,
            exposure_private: false,
            exposure_sources: Vec::new(),
        })?;
        saved += 1;
    }
    Ok((StatusCode::CREATED, Json(VoiceTranscriptOutput { saved })))
}
