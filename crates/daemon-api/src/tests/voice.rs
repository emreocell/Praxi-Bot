//! `/v1/voice*` against an in-memory `VoiceControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::json;

use super::{T, call, setup};
use crate::voice::{
    TranscribeOutput, VoiceConfigDto, VoiceConfigInput, VoiceControl, VoiceControlError,
};

const SECRET: &str = "sk-voice-writeonly-secret";

struct FakeVoice {
    key: Mutex<Option<String>>,
    last_audio_len: Mutex<usize>,
}

#[async_trait]
impl VoiceControl for FakeVoice {
    fn get(&self) -> VoiceConfigDto {
        VoiceConfigDto {
            stt_provider: crate::voice::VoiceProvider::Whisper,
            gemini_model: "gemini-3.5-transcribe-live".into(),
            gemini_key_set: false,
            stt_base_url: "https://stt.example/v1".into(),
            stt_model: "whisper-1".into(),
            stt_key_set: self.key.lock().is_some(),
        }
    }

    async fn update(&self, input: VoiceConfigInput) -> Result<VoiceConfigDto, VoiceControlError> {
        if let Some(k) = input.stt_key.or(input.gemini_key) {
            *self.key.lock() = Some(k);
        }
        Ok(self.get())
    }

    async fn transcribe(
        &self,
        audio: Vec<u8>,
        _mime: String,
        _language: Option<String>,
    ) -> Result<TranscribeOutput, VoiceControlError> {
        *self.last_audio_len.lock() = audio.len();
        if self.key.lock().is_none() {
            return Err(VoiceControlError::NotConfigured);
        }
        Ok(TranscribeOutput {
            text: "hello world".into(),
        })
    }
}

fn with_voice() -> (T, Arc<FakeVoice>) {
    let t = setup();
    let v = Arc::new(FakeVoice {
        key: Mutex::new(None),
        last_audio_len: Mutex::new(0),
    });
    t.state.set_voice(Arc::clone(&v) as Arc<dyn VoiceControl>);
    (t, v)
}

#[tokio::test]
async fn voice_key_is_write_only_and_transcribe_validates_input() {
    let (t, v) = with_voice();

    // Not configured yet: 409, not a crash.
    let body = json!({"audio_base64": "AAEC", "mime": "audio/webm"});
    let r = call(&t, Method::POST, "/v1/voice/transcribe", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // The key goes in, never comes back.
    let r = call(
        &t,
        Method::PUT,
        "/v1/voice",
        Some(json!({"stt_key": SECRET})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&r.body).contains(SECRET));
    assert_eq!(r.json()["stt_key_set"], json!(true));

    // Audio is decoded and handed over; bad input is rejected before the service is called.
    let r = call(&t, Method::POST, "/v1/voice/transcribe", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["text"], json!("hello world"));
    assert_eq!(*v.last_audio_len.lock(), 3);
    for bad in [
        json!({"audio_base64": "AAEC", "mime": "text/plain"}),
        json!({"audio_base64": "!!!", "mime": "audio/webm"}),
    ] {
        let r = call(&t, Method::POST, "/v1/voice/transcribe", Some(bad)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn gemini_key_is_validated_and_never_echoed() {
    let (t, _v) = with_voice();
    let r = call(
        &t,
        Method::PUT,
        "/v1/voice",
        Some(json!({"stt_provider": "gemini_live", "gemini_key": "AIza-secret\nx"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &t,
        Method::PUT,
        "/v1/voice",
        Some(json!({"stt_provider": "gemini_live", "gemini_key": "AIza-gemini-secret"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&r.body).contains("AIza-gemini-secret"));
}
