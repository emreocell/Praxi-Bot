//! `/v1/voice*` against an in-memory `VoiceControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::json;

use super::{T, call, call_token_only, setup};
use crate::voice::{
    SynthesizeInput, TranscribeOutput, VoiceConfigDto, VoiceConfigInput, VoiceControl,
    VoiceControlError, VoiceProvider,
};

const SECRET: &str = "sk-voice-writeonly-secret";

struct FakeVoice {
    key: Mutex<Option<String>>,
    last_audio_len: Mutex<usize>,
    provider: Mutex<VoiceProvider>,
    last_local_only: Mutex<Option<bool>>,
    last_synthesis: Mutex<Option<SynthesizeInput>>,
}

const WAV: &[u8] = b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\xc0\x5d\0\0\x80\xbb\0\0\x02\0\x10\0data\x02\0\0\0\0\0";

#[async_trait]
impl VoiceControl for FakeVoice {
    fn get(&self) -> VoiceConfigDto {
        VoiceConfigDto {
            stt_provider: *self.provider.lock(),
            gemini_model: "gemini-3.5-transcribe-live".into(),
            gemini_key_set: false,
            use_provider_key: true,
            gemini_provider_key_available: false,
            stt_provider_key_available: false,
            local_model: "whisper-small".into(),
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
        local_only: bool,
    ) -> Result<TranscribeOutput, VoiceControlError> {
        *self.last_local_only.lock() = Some(local_only);
        *self.last_audio_len.lock() = audio.len();
        if self.key.lock().is_none() {
            return Err(VoiceControlError::NotConfigured);
        }
        Ok(TranscribeOutput {
            text: "hello world".into(),
        })
    }

    async fn synthesize(&self, input: SynthesizeInput) -> Result<Vec<u8>, VoiceControlError> {
        *self.last_synthesis.lock() = Some(input);
        Ok(WAV.to_vec())
    }
}

fn with_voice() -> (T, Arc<FakeVoice>) {
    let t = setup();
    let v = Arc::new(FakeVoice {
        key: Mutex::new(None),
        last_audio_len: Mutex::new(0),
        provider: Mutex::new(VoiceProvider::Whisper),
        last_local_only: Mutex::new(None),
        last_synthesis: Mutex::new(None),
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

#[tokio::test]
async fn local_voice_routes_default_to_unavailable_and_install_is_owner_only() {
    let (t, _v) = with_voice();
    let r = call(&t, Method::GET, "/v1/voice/local", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["available"], json!(false));
    // The embedder says no: a plain 422, never a download.
    let r = call(
        &t,
        Method::POST,
        "/v1/voice/local/install",
        Some(json!({"model": "whisper-small"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        crate::owner::OWNER_ROUTES
            .iter()
            .any(|(m, p)| *m == "POST" && *p == "/v1/voice/local/install")
    );
}

#[tokio::test]
async fn offline_synthesis_accepts_local_owner_and_returns_uncached_wav() {
    const ROUTE: &str = "/v1/voice/synthesize";
    let (t, v) = with_voice();
    let body = json!({"text": "  Günaydın.  ", "language": "tr-TR"});
    let denied = call_token_only(&t, Method::POST, ROUTE, Some(body.clone())).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert!(crate::peer::admin_desktop_never(&Method::POST, ROUTE).is_some());
    assert!(v.last_synthesis.lock().is_none());
    for text in ["\0".to_owned(), " ".to_owned(), "ğ".repeat(1001)] {
        let bad = call(
            &t,
            Method::POST,
            ROUTE,
            Some(json!({"text": text, "language": "tr"})),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }
    assert!(v.last_synthesis.lock().is_none());
    let response = call(&t, Method::POST, ROUTE, Some(body)).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers["content-type"], "audio/wav");
    assert_eq!(response.headers["cache-control"], "no-store");
    assert_eq!(response.body, WAV);
    let input = v.last_synthesis.lock().take().unwrap();
    assert_eq!(input.text, "Günaydın.");
    assert_eq!(input.language, "tr-TR");
}

#[tokio::test]
async fn local_only_reaches_the_host_and_dictation_on_device_is_not_for_peers() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    let (t, v) = with_voice();
    v.update(VoiceConfigInput {
        stt_key: Some(SECRET.into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let body = json!({"audio_base64": "AAEC", "mime": "audio/pcm", "local_only": true});
    let r = call(&t, Method::POST, "/v1/voice/transcribe", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        *v.last_local_only.lock(),
        Some(true),
        "the flag is not dropped"
    );
    let plain = json!({"audio_base64": "AAEC", "mime": "audio/pcm"});
    call(&t, Method::POST, "/v1/voice/transcribe", Some(plain)).await;
    assert_eq!(*v.last_local_only.lock(), Some(false));

    // A peer may use a cloud dictation service, never the host's own recogniser.
    let as_peer = |role: crate::PeerRole| {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/voice/transcribe")
            .header("content-type", "application/json")
            .extension(crate::PeerIdentity {
                key: "peer-key".into(),
                role,
            })
            .body(Body::from(body.to_string()))
            .unwrap();
        crate::peer_router(t.state.clone()).oneshot(req)
    };
    for role in [crate::PeerRole::Phone, crate::PeerRole::Desktop] {
        *v.last_local_only.lock() = None;
        let ok = as_peer(role).await.unwrap().status();
        assert_ne!(ok, StatusCode::FORBIDDEN, "{role:?}");
        assert_eq!(*v.last_local_only.lock(), Some(true), "{role:?}");
    }
    *v.provider.lock() = VoiceProvider::LocalVoice;
    *v.last_local_only.lock() = None;
    for role in [
        crate::PeerRole::Phone,
        crate::PeerRole::Desktop,
        crate::PeerRole::Node,
        crate::PeerRole::Host,
    ] {
        let status = as_peer(role).await.unwrap().status();
        assert_eq!(status, StatusCode::FORBIDDEN, "{role:?}");
    }
    assert_eq!(
        *v.last_local_only.lock(),
        None,
        "no peer reached the engine"
    );
    // The owner on this computer still dictates.
    let r = call(&t, Method::POST, "/v1/voice/transcribe", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
}
