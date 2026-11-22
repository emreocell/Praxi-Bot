//! Handlers of `/v1/voice*`. Same guard (Host / Origin / bearer) as every other route.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::routes::ApiJson;
use crate::voice::{
    LocalVoiceInstallInput, LocalVoiceStatusDto, TranscribeInput, TranscribeOutput, VoiceConfigDto,
    VoiceConfigInput, VoiceControl, VoiceControlError,
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
#[utoipa::path(post, path = "/v1/voice/transcribe", tag = "voice", security(("bearer" = [])),
    request_body = TranscribeInput,
    responses((status = 200, body = TranscribeOutput),
              (status = 400, body = ErrorBody, description = "bad or oversized audio"),
              (status = 409, body = ErrorBody, description = "dictation is not set up"),
              (status = 502, body = ErrorBody, description = "the transcription service failed"),
              (status = 503, body = ErrorBody, description = "voice is not available")))]
pub(crate) async fn transcribe(
    State(s): State<AppState>,
    ApiJson(input): ApiJson<TranscribeInput>,
) -> Result<Json<TranscribeOutput>, ApiError> {
    let control = voice(&s)?;
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
            .transcribe(audio, input.mime, input.language)
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
    Ok(Json(voice(&s)?.local_status()))
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
