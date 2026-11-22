//! Speech-to-text for dictation (`/v1/voice*`, ADR 0021): DTOs and the [`VoiceControl`] trait the
//! embedder implements (`apps/cli/src/voice_host.rs`).
//!
//! Recorded audio is sent, on the owner's explicit action, to an OpenAI-compatible
//! `/audio/transcriptions` endpoint the owner configured (a local server or a cloud provider).
//! The API key is **write-only**: it goes into the secret store and no response carries it.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which service turns speech into text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProvider {
    /// An OpenAI-compatible `/audio/transcriptions` service (cloud or local). Takes `MediaRecorder` audio.
    #[default]
    Whisper,
    /// Gemini Live API (server-side WebSocket, input transcription only). Takes `audio/pcm` 16 kHz mono.
    GeminiLive,
}

/// Dictation configuration (`GET /v1/voice`): non-secret fields only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VoiceConfigDto {
    /// The active speech-to-text provider.
    pub stt_provider: VoiceProvider,
    /// Gemini Live model used for transcription (e.g. `gemini-3.5-transcribe-live`).
    pub gemini_model: String,
    /// Whether a Gemini API key is stored (never returned).
    pub gemini_key_set: bool,
    /// Base URL of the OpenAI-compatible transcription service (without `/audio/transcriptions`).
    pub stt_base_url: String,
    /// Model name sent with each request.
    pub stt_model: String,
    /// Whether an API key is stored (never returned).
    pub stt_key_set: bool,
}

/// Body of `PUT /v1/voice`; omitted fields keep their value.
#[derive(Clone, Default, Deserialize, ToSchema)]
pub struct VoiceConfigInput {
    /// Switch the provider.
    #[serde(default)]
    pub stt_provider: Option<VoiceProvider>,
    /// Gemini Live model name.
    #[serde(default)]
    pub gemini_model: Option<String>,
    /// Gemini API key (**write-only**: secret store, never returned or logged).
    #[serde(default)]
    pub gemini_key: Option<String>,
    /// Remove the stored Gemini key.
    #[serde(default)]
    pub clear_gemini_key: Option<bool>,
    /// `https://...`, or `http://` for a loopback server only.
    #[serde(default)]
    pub stt_base_url: Option<String>,
    /// Model name.
    #[serde(default)]
    pub stt_model: Option<String>,
    /// API key (**write-only**: stored in the secret store, never returned or logged).
    #[serde(default)]
    pub stt_key: Option<String>,
    /// Remove the stored key.
    #[serde(default)]
    pub clear_stt_key: Option<bool>,
}

impl std::fmt::Debug for VoiceConfigInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceConfigInput")
            .field("stt_provider", &self.stt_provider)
            .field("gemini_model", &self.gemini_model)
            .field(
                "gemini_key",
                &self.gemini_key.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_gemini_key", &self.clear_gemini_key)
            .field("stt_base_url", &self.stt_base_url)
            .field("stt_model", &self.stt_model)
            .field("stt_key", &self.stt_key.as_ref().map(|_| "<redacted>"))
            .field("clear_stt_key", &self.clear_stt_key)
            .finish()
    }
}

/// Body of `POST /v1/voice/transcribe`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TranscribeInput {
    /// The recording, base64 (standard alphabet).
    pub audio_base64: String,
    /// MIME type of the recording, e.g. `audio/webm` (Whisper) or `audio/pcm` (Gemini Live: 16-bit LE, 16 kHz, mono).
    pub mime: String,
    /// Optional ISO-639-1 language hint (`en`, `tr`, `de`).
    #[serde(default)]
    pub language: Option<String>,
}

/// Result of `POST /v1/voice/transcribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TranscribeOutput {
    /// The recognised text.
    pub text: String,
}

/// Why a [`VoiceControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceControlError {
    /// Bad configuration or input (`422`).
    Invalid(String),
    /// Dictation is not configured yet (`409`).
    NotConfigured,
    /// The transcription service failed (`502`).
    Upstream(String),
    /// Persistence / secret-store failure (`500`).
    Internal(String),
}

/// What `/v1/voice*` needs from the embedder.
#[async_trait]
pub trait VoiceControl: Send + Sync {
    /// Current configuration (non-secret fields only).
    fn get(&self) -> VoiceConfigDto;

    /// Validate and persist the configuration (key to the secret store).
    ///
    /// # Errors
    /// [`VoiceControlError`].
    async fn update(&self, input: VoiceConfigInput) -> Result<VoiceConfigDto, VoiceControlError>;

    /// Transcribe one recording.
    ///
    /// # Errors
    /// [`VoiceControlError`].
    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: String,
        language: Option<String>,
    ) -> Result<TranscribeOutput, VoiceControlError>;
}
