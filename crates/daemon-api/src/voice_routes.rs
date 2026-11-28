//! Handlers of `/v1/voice*`. Same guard (Host / Origin / bearer) as every other route.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use base64::Engine as _;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::routes::ApiJson;
use crate::voice::{
    LocalVoiceInstallInput, LocalVoiceStatusDto, SynthesizeInput, TranscribeInput,
    TranscribeOutput, VoiceConfigDto, VoiceConfigInput, VoiceControl, VoiceControlError,
    VoiceProvider,
};

/// Longest accepted API key (bytes).
const MAX_KEY_BYTES: usize = 1024;
/// Largest accepted recording after decoding (bytes); the `OpenAI` limit is 25 MB.
pub(crate) const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
/// Request body limit: base64 of the largest recording plus the JSON frame.
pub(crate) const MAX_BODY_BYTES: usize = MAX_AUDIO_BYTES / 3 * 4 + 4096;

impl From<VoiceControlError> for ApiError {
    fn from(e: VoiceControlError) -> Self {
        match e {
            VoiceControlError::Invalid(m) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_voice_config", m)
            }
            VoiceControlError::NotConfigured => Self::new(
                StatusCode::CONFLICT,
                "voice_not_configured",
                "dictation is not set up: add a transcription service in Settings",
            ),
            VoiceControlError::Busy => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "another local voice request is running",
            ),
            VoiceControlError::Upstream(m) => {
                Self::new(StatusCode::BAD_GATEWAY, "voice_upstream_failed", m)
            }
            VoiceControlError::Internal(m) => Self::internal(m),
        }
    }
}

fn voice(s: &AppState) -> Result<Arc<dyn VoiceControl>, ApiError> {
    s.voice()
        .ok_or_else(|| ApiError::unavailable("voice is not available in this daemon"))
}

/// Dictation configuration; the API key is write-only (`stt_key_set` only says whether one is stored).
#[utoipa::path(get, path = "/v1/voice", tag = "voice", security(("bearer" = [])),
    responses((status = 200, body = VoiceConfigDto),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn get_voice(State(s): State<AppState>) -> Result<Json<VoiceConfigDto>, ApiError> {
    Ok(Json(voice(&s)?.get()))
}

/// Update the dictation configuration.
#[utoipa::path(put, path = "/v1/voice", tag = "voice", security(("bearer" = [])),
    request_body = VoiceConfigInput,
    responses((status = 200, body = VoiceConfigDto),
              (status = 400, body = ErrorBody, description = "bad API key"),
              (status = 422, body = ErrorBody, description = "invalid config"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn update_voice(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<VoiceConfigInput>,
) -> Result<Json<VoiceConfigDto>, ApiError> {
    let control = voice(&s)?;
    if let Some(k) = &input.stt_key
        && (k.is_empty() || k.len() > MAX_KEY_BYTES || k.contains(['\0', '\r', '\n']))
    {
        return Err(ApiError::bad_request(format!(
            "`stt_key` must be 1 to {MAX_KEY_BYTES} bytes without line breaks or NUL"
        )));
    }
    if let Some(k) = &input.gemini_key
        && (k.is_empty() || k.len() > MAX_KEY_BYTES || k.contains(['\0', '\r', '\n']))
    {
        return Err(ApiError::bad_request(format!(
            "`gemini_key` must be 1 to {MAX_KEY_BYTES} bytes without line breaks or NUL"
        )));
    }
    Ok(Json(control.update(input).await?))
}

/// Transcribe a recording with the configured service (owner-triggered dictation).
///
/// With `local_only` the daemon refuses (409) unless the provider is on-device, so the recording
/// is never uploaded. A paired peer cannot use the host's on-device engine (403): decoding and
/// recognition burn the host's CPU for up to two minutes and would starve the owner's own dictation.
#[utoipa::path(post, path = "/v1/voice/transcribe", tag = "voice", security(("bearer" = [])),
    request_body = TranscribeInput,
    responses((status = 200, body = TranscribeOutput),
              (status = 400, body = ErrorBody, description = "bad or oversized audio"),
              (status = 403, body = ErrorBody, description = "on-device dictation is for the local owner"),
              (status = 409, body = ErrorBody, description = "dictation is not set up, or `local_only` and the provider is not on-device"),
              (status = 429, body = ErrorBody, description = "offline voice is busy"),
              (status = 502, body = ErrorBody, description = "the transcription service failed"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn transcribe(
    State(s): State<AppState>,
    peer: Option<Extension<crate::PeerIdentity>>,
    ApiJson(input): ApiJson<TranscribeInput>,
) -> Result<Json<TranscribeOutput>, ApiError> {
    let control = voice(&s)?;
    if peer.is_some() && control.get().stt_provider == VoiceProvider::LocalVoice {
        return Err(ApiError::forbidden(
            "on-device dictation runs on the computer that owns it",
        ));
    }
    if !input.mime.starts_with("audio/") || input.mime.len() > 100 {
        return Err(ApiError::bad_request("`mime` must be an audio/* type"));
    }
    let audio = base64::engine::general_purpose::STANDARD
        .decode(input.audio_base64.as_bytes())
        .map_err(|_| ApiError::bad_request("`audio_base64` is not valid base64"))?;
    if audio.is_empty() || audio.len() > MAX_AUDIO_BYTES {
        return Err(ApiError::bad_request(format!(
            "the recording must be 1 byte to {MAX_AUDIO_BYTES} bytes"
        )));
    }
    Ok(Json(
        control
            .transcribe(audio, input.mime, input.language, input.local_only)
            .await?,
    ))
}

/// On-device speech models: what can be installed, licences and install state.
#[utoipa::path(get, path = "/v1/voice/local", tag = "voice", security(("bearer" = [])),
    responses((status = 200, body = LocalVoiceStatusDto),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn get_local_voice(
    State(s): State<AppState>,
) -> Result<Json<LocalVoiceStatusDto>, ApiError> {
    let control = voice(&s)?;
    tokio::task::spawn_blocking(move || Json(control.local_status()))
        .await
        .map_err(|_| ApiError::internal("local voice status task failed"))
}

/// Download one on-device speech model (owner only; checksum-verified, returns at once).
#[utoipa::path(post, path = "/v1/voice/local/install", tag = "voice", security(("bearer" = [])),
    request_body = LocalVoiceInstallInput,
    responses((status = 200, body = LocalVoiceStatusDto),
              (status = 422, body = ErrorBody, description = "unknown model or not available"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn install_local_voice(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<LocalVoiceInstallInput>,
) -> Result<Json<LocalVoiceStatusDto>, ApiError> {
    if input.model.len() > 64 {
        return Err(ApiError::bad_request("`model` is too long"));
    }
    Ok(Json(voice(&s)?.local_install(input.model).await?))
}

/// Synthesize speech on this computer (local owner only).
#[utoipa::path(post, path = "/v1/voice/synthesize", tag = "voice", security(("bearer" = [])),
    request_body = SynthesizeInput,
    responses((status = 200, body = Vec<u8>, content_type = "audio/wav"),
              (status = 400, body = ErrorBody, description = "invalid text or language"),
              (status = 403, body = ErrorBody, description = "local owner required"),
              (status = 409, body = ErrorBody, description = "offline read-aloud is not installed"),
              (status = 429, body = ErrorBody, description = "offline voice is busy"),
              (status = 502, body = ErrorBody, description = "offline synthesis failed"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn synthesize(
    State(s): State<AppState>,
    ApiJson(mut input): ApiJson<SynthesizeInput>,
) -> Result<impl IntoResponse, ApiError> {
    input.validate().map_err(|e| match e {
        VoiceControlError::Invalid(m) => ApiError::bad_request(m),
        other => other.into(),
    })?;
    let audio = voice(&s)?.synthesize(input).await.map_err(|e| match e {
        VoiceControlError::NotConfigured => ApiError::new(
            StatusCode::CONFLICT,
            "voice_not_configured",
            "download the offline read-aloud model in Settings",
        ),
        other => other.into(),
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        audio,
    ))
}
