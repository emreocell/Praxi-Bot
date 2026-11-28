//! Speech-to-text and local speech synthesis (`/v1/voice*`, ADR 0021): DTOs and the [`VoiceControl`] trait the
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
    /// On-device speech-to-text (Whisper + Silero VAD through sherpa-onnx). Audio never leaves
    /// the machine. Needs a daemon built with the `local-voice` feature and a downloaded model.
    LocalVoice,
}

/// Dictation configuration (`GET /v1/voice`): non-secret fields only.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VoiceConfigDto {
    /// The active speech-to-text provider.
    pub stt_provider: VoiceProvider,
    /// Gemini Live model used for transcription (e.g. `gemini-3.5-transcribe-live`).
    pub gemini_model: String,
    /// Whether a Gemini API key is stored (never returned).
    pub gemini_key_set: bool,
    /// Id of the on-device speech model used when the provider is `local_voice`.
    pub local_model: String,
    /// Base URL of the OpenAI-compatible transcription service (without `/audio/transcriptions`).
    pub stt_base_url: String,
    /// Model name sent with each request.
    pub stt_model: String,
    /// Whether an API key is stored (never returned).
    pub stt_key_set: bool,
    /// Reuse the key of a matching provider from Bots & models instead of a separate voice
    /// key (default on). The key itself never leaves the secret store.
    pub use_provider_key: bool,
    /// A Gemini provider with a key exists under Bots & models.
    pub gemini_provider_key_available: bool,
    /// A provider under Bots & models has a key and the same base URL as `stt_base_url`.
    pub stt_provider_key_available: bool,
    /// Gemini Live model used for live voice conversations (e.g. `gemini-3.8-live`).
    pub live_model: String,
    /// Prebuilt voice the live model speaks with (e.g. `Kore`).
    pub live_voice: String,
    /// Keep the audio of live conversations on this computer (default off).
    pub live_keep_audio: bool,
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
    /// Id of an on-device speech model from `GET /v1/voice/local`.
    #[serde(default)]
    pub local_model: Option<String>,
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
    /// Reuse the matching provider key from Bots & models (default on).
    #[serde(default)]
    pub use_provider_key: Option<bool>,
    /// Gemini Live model for live voice conversations.
    #[serde(default)]
    pub live_model: Option<String>,
    /// Prebuilt voice name for live voice conversations.
    #[serde(default)]
    pub live_voice: Option<String>,
    /// Keep the audio of live conversations on this computer.
    #[serde(default)]
    pub live_keep_audio: Option<bool>,
}

impl std::fmt::Debug for VoiceConfigInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceConfigInput")
            .field("stt_provider", &self.stt_provider)
            .field("gemini_model", &self.gemini_model)
            .field("local_model", &self.local_model)
            .field(
                "gemini_key",
                &self.gemini_key.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_gemini_key", &self.clear_gemini_key)
            .field("stt_base_url", &self.stt_base_url)
            .field("stt_model", &self.stt_model)
            .field("stt_key", &self.stt_key.as_ref().map(|_| "<redacted>"))
            .field("clear_stt_key", &self.clear_stt_key)
            .field("use_provider_key", &self.use_provider_key)
            .field("live_model", &self.live_model)
            .field("live_voice", &self.live_voice)
            .field("live_keep_audio", &self.live_keep_audio)
            .finish()
    }
}

/// One downloadable on-device speech model (`GET /v1/voice/local`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalVoiceModelDto {
    /// Stable id, e.g. `whisper-small`.
    pub id: String,
    /// Short name for a picker.
    pub label: String,
    /// One plain sentence: what it is good for.
    pub description: String,
    /// Download size in megabytes.
    pub size_mb: u32,
    /// Licence of the model weights (SPDX-style), shown before the download.
    pub license: String,
    /// Where the licence text lives.
    pub license_url: String,
    /// Where the files are downloaded from (host only).
    pub source_host: String,
    /// `true` when every file is on disk and its checksum was verified.
    pub installed: bool,
}

/// On-device speech state (`GET /v1/voice/local`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalVoiceStatusDto {
    /// `false` when this daemon was built without the `local-voice` feature.
    pub available: bool,
    /// Whether the voice-activity model (end-of-speech detection) is installed.
    pub vad_installed: bool,
    /// Models the owner can install.
    pub models: Vec<LocalVoiceModelDto>,
    /// Optional downloadable model for offline read-aloud.
    pub tts_model: Option<LocalVoiceModelDto>,
    /// Id of the model being downloaded right now, if any.
    pub installing: Option<String>,
    /// Plain-language reason the last download failed, if it did.
    pub last_error: Option<String>,
}

/// Body of `POST /v1/voice/local/install`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LocalVoiceInstallInput {
    /// A model id from `GET /v1/voice/local`.
    pub model: String,
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
    /// Refuse (409) unless the configured provider is on-device: the audio is never uploaded.
    #[serde(default)]
    pub local_only: bool,
}

/// Result of `POST /v1/voice/transcribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TranscribeOutput {
    /// The recognised text.
    pub text: String,
}

/// Text and app language for local synthesis (`POST /v1/voice/synthesize`).
#[derive(Clone, Deserialize, ToSchema)]
pub struct SynthesizeInput {
    /// Plain text, at most 1,000 Unicode characters.
    pub text: String,
    /// App language (BCP 47, e.g. `tr` or `en-US`).
    pub language: String,
}

/// Longest text one synthesis request may carry (Unicode characters).
pub const MAX_SYNTHESIS_CHARS: usize = 1_000;
/// Longest accepted BCP 47 language tag (bytes).
pub const MAX_LANGUAGE_TAG_BYTES: usize = 35;

impl SynthesizeInput {
    /// Trim the text and check the limits: 1 to [`MAX_SYNTHESIS_CHARS`] characters without NUL,
    /// and a language tag of ASCII letters, digits and `-` (at most [`MAX_LANGUAGE_TAG_BYTES`]).
    ///
    /// This is the one rule every layer applies (the route, the host and the worker process), so
    /// a caller that skips the route cannot make the worker synthesize an unbounded text.
    ///
    /// # Errors
    /// [`VoiceControlError::Invalid`] with a plain-language reason.
    pub fn validate(&mut self) -> Result<(), VoiceControlError> {
        self.text = self.text.trim().to_owned();
        if self.text.is_empty()
            || self.text.contains('\0')
            || self.text.chars().count() > MAX_SYNTHESIS_CHARS
        {
            return Err(VoiceControlError::Invalid(format!(
                "text must be 1 to {MAX_SYNTHESIS_CHARS} characters without NUL"
            )));
        }
        if self.language.is_empty()
            || self.language.len() > MAX_LANGUAGE_TAG_BYTES
            || !self
                .language
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(VoiceControlError::Invalid(
                "language must be a BCP 47 language tag".into(),
            ));
        }
        Ok(())
    }
}

/// Why a [`VoiceControl`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceControlError {
    /// Bad configuration or input (`422`).
    Invalid(String),
    /// Dictation is not configured yet (`409`).
    NotConfigured,
    /// Another local voice request is already using the inference process (`429`).
    Busy,
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
        local_only: bool,
    ) -> Result<TranscribeOutput, VoiceControlError>;

    /// On-device speech models and their install state. The default says "not available".
    fn local_status(&self) -> LocalVoiceStatusDto {
        LocalVoiceStatusDto {
            available: false,
            vad_installed: false,
            models: Vec::new(),
            tts_model: None,
            installing: None,
            last_error: None,
        }
    }

    /// Open a live voice conversation. The default says "not available".
    ///
    /// # Errors
    /// [`VoiceControlError`] (`NotConfigured` without a Gemini key).
    async fn live_open(
        &self,
        _open: crate::voice_live::LiveOpen,
    ) -> Result<crate::voice_live::LiveLink, VoiceControlError> {
        Err(VoiceControlError::Invalid(
            "live voice is not available in this build".into(),
        ))
    }

    /// Start downloading one on-device model (returns at once; poll [`Self::local_status`]).
    ///
    /// # Errors
    /// [`VoiceControlError`].
    async fn local_install(
        &self,
        _model: String,
    ) -> Result<LocalVoiceStatusDto, VoiceControlError> {
        Err(VoiceControlError::Invalid(
            "on-device voice is not available in this build".into(),
        ))
    }

    /// Synthesize mono PCM WAV without contacting a provider.
    ///
    /// # Errors
    /// [`VoiceControlError`].
    async fn synthesize(&self, _input: SynthesizeInput) -> Result<Vec<u8>, VoiceControlError> {
        Err(VoiceControlError::NotConfigured)
    }

    /// Local voice health for doctor. Runs on a blocking thread.
    fn offline_check(&self) -> crate::doctor::Check {
        offline_voice_unavailable()
    }
}

pub(crate) fn offline_voice_unavailable() -> crate::doctor::Check {
    crate::doctor::Check::skipped(
        "offline_voice",
        "offline voice",
        "this build does not include offline voice",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str, language: &str) -> SynthesizeInput {
        SynthesizeInput {
            text: text.into(),
            language: language.into(),
        }
    }

    #[test]
    fn synthesis_input_is_trimmed_and_bounded() {
        let mut ok = input("  Günaydın.  ", "tr-TR");
        ok.validate().unwrap();
        assert_eq!(ok.text, "Günaydın.");
        input(&"ğ".repeat(MAX_SYNTHESIS_CHARS), "es-419")
            .validate()
            .unwrap();
        for (text, language) in [
            (String::new(), "tr"),
            ("   ".to_owned(), "tr"),
            ("a\0b".to_owned(), "tr"),
            ("ğ".repeat(MAX_SYNTHESIS_CHARS + 1), "tr"),
            // a 32 KiB worker header would otherwise carry about 30,000 characters
            ("a".repeat(30_000), "tr"),
            ("hello".to_owned(), ""),
            ("hello".to_owned(), "en_US"),
            ("hello".to_owned(), "en US"),
            ("hello".to_owned(), "../x"),
            ("hello".to_owned(), "ğ"),
            ("hello".to_owned(), &"a".repeat(MAX_LANGUAGE_TAG_BYTES + 1)),
        ] {
            let err = input(&text, language).validate().unwrap_err();
            assert!(matches!(err, VoiceControlError::Invalid(_)), "{err:?}");
        }
    }
}
