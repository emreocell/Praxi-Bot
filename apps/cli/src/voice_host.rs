//! Embedder backing `/v1/voice*` (`daemon_api::VoiceControl`): dictation through an
//! OpenAI-compatible `/audio/transcriptions` service. Non-secret settings live in
//! `<data_dir>/voice.json`; the API key lives in the secret store (`voice/stt`).
//!
//! Audio leaves the machine only when the owner presses the microphone button, and only to the
//! endpoint the owner configured. `http://` is accepted for loopback servers only (a local
//! whisper server), never for the daemon's own port.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use daemon_api::voice::{
    LocalVoiceStatusDto, TranscribeOutput, VoiceConfigDto, VoiceConfigInput, VoiceControl,
    VoiceControlError, VoiceProvider,
};
use provider_catalog::{ProviderConfig, ProviderSettings, config_path};
use secret_store::SecretStore;
use serde::{Deserialize, Serialize};

const VOICE_FILE: &str = "voice.json";
const KEY_NAME: &str = "voice/stt";
const GEMINI_KEY_NAME: &str = "voice/gemini";
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-transcribe-live";
/// Longest accepted Gemini utterance: 120 s of 16 kHz 16-bit mono PCM.
const MAX_GEMINI_PCM_BYTES: usize = 120 * 32_000;
/// Catalog providers whose key is a Gemini API key.
const GEMINI_PROVIDER_IDS: [&str; 2] = ["gemini", "gemini-openai-compat"];
const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "whisper-1";
const MAX_FIELD_CHARS: usize = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VoiceFile {
    #[serde(default = "default_base_url")]
    stt_base_url: String,
    #[serde(default = "default_model")]
    stt_model: String,
    #[serde(default)]
    stt_provider: VoiceProvider,
    #[serde(default = "default_gemini_model")]
    gemini_model: String,
    #[serde(default = "default_local_model")]
    local_model: String,
    /// Reuse a matching provider key from Bots & models (default on).
    #[serde(default = "default_true")]
    use_provider_key: bool,
}

fn default_true() -> bool {
    true
}

fn default_local_model() -> String {
    crate::voice_local::DEFAULT_MODEL.to_owned()
}

fn default_gemini_model() -> String {
    DEFAULT_GEMINI_MODEL.to_owned()
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_owned()
}

fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}

impl Default for VoiceFile {
    fn default() -> Self {
        Self {
            stt_base_url: default_base_url(),
            stt_model: default_model(),
            stt_provider: VoiceProvider::default(),
            gemini_model: default_gemini_model(),
            local_model: default_local_model(),
            use_provider_key: true,
        }
    }
}

/// Manages `voice.json`, the transcription key and the upload itself.
pub struct VoiceHost {
    data_dir: PathBuf,
    daemon_port: u16,
    secrets: Option<Arc<dyn SecretStore>>,
    http: reqwest::Client,
    update_lock: tokio::sync::Mutex<()>,
    gemini_ws_base: String,
    local: crate::voice_local::LocalVoice,
    #[cfg(feature = "local-voice")]
    engine: Arc<crate::voice_local_engine::Engine>,
}

impl VoiceHost {
    /// A controller bound to `data_dir`.
    #[must_use]
    pub fn new(data_dir: &Path, daemon_port: u16, secrets: Option<Arc<dyn SecretStore>>) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            daemon_port,
            secrets,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            update_lock: tokio::sync::Mutex::new(()),
            gemini_ws_base: crate::voice_gemini::DEFAULT_WS_BASE.to_owned(),
            local: crate::voice_local::LocalVoice::new(data_dir),
            #[cfg(feature = "local-voice")]
            engine: Arc::default(),
        }
    }

    fn load(&self) -> VoiceFile {
        std::fs::read_to_string(self.data_dir.join(VOICE_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, file: &VoiceFile) -> Result<(), VoiceControlError> {
        let path = self.data_dir.join(VOICE_FILE);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(file).map_err(|e| {
            VoiceControlError::Internal(format!("cannot serialize voice.json: {e}"))
        })?;
        let write = || -> std::io::Result<()> {
            std::fs::write(&tmp, &body)?;
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            VoiceControlError::Internal(format!("cannot save voice.json: {e}"))
        })
    }

    fn key(&self) -> Option<String> {
        self.secret(KEY_NAME)
    }

    fn secret(&self, name: &str) -> Option<String> {
        let store = self.secrets.as_ref()?;
        store
            .get(name)
            .ok()
            .flatten()
            .map(|v| v.expose().to_owned())
            .filter(|k| !k.trim().is_empty())
    }

    /// The key of a configured, enabled provider (Bots & models) accepted by `matches`.
    /// Read from the secret store; callers use it only for the provider's own host.
    fn provider_key(&self, matches: impl Fn(&ProviderSettings) -> bool) -> Option<String> {
        let cfg = ProviderConfig::load(&config_path(&self.data_dir)).ok()?;
        cfg.providers
            .iter()
            .filter(|p| p.enabled && matches(p))
            .find_map(|p| self.secret(p.key.as_ref()?.name()))
    }

    fn gemini_provider_key(&self) -> Option<String> {
        self.provider_key(|p| GEMINI_PROVIDER_IDS.contains(&p.id.as_str()))
    }

    /// Key of a provider whose base URL is exactly the dictation URL, so the key is only ever
    /// sent to the host the owner gave it to.
    fn stt_provider_key(&self, base_url: &str) -> Option<String> {
        let want = base_url.trim_end_matches('/');
        self.provider_key(|p| {
            let url = match (&p.custom, p.id.as_str()) {
                (Some(entry), _) => entry.base_url.as_str(),
                (None, "groq") => GROQ_BASE_URL,
                _ => return false,
            };
            url.trim_end_matches('/') == want
        })
    }

    /// The Gemini key to use: the Bots & models key when reuse is on, else the voice key.
    fn effective_gemini_key(&self, file: &VoiceFile) -> Option<String> {
        file.use_provider_key
            .then(|| self.gemini_provider_key())
            .flatten()
            .or_else(|| self.secret(GEMINI_KEY_NAME))
    }

    fn effective_stt_key(&self, file: &VoiceFile) -> Option<String> {
        file.use_provider_key
            .then(|| self.stt_provider_key(&file.stt_base_url))
            .flatten()
            .or_else(|| self.key())
    }

    fn dto(&self, file: &VoiceFile) -> VoiceConfigDto {
        VoiceConfigDto {
            stt_base_url: file.stt_base_url.clone(),
            stt_model: file.stt_model.clone(),
            stt_key_set: self.key().is_some(),
            stt_provider: file.stt_provider,
            gemini_model: file.gemini_model.clone(),
            local_model: file.local_model.clone(),
            gemini_key_set: self.secret(GEMINI_KEY_NAME).is_some(),
            use_provider_key: file.use_provider_key,
            gemini_provider_key_available: self.gemini_provider_key().is_some(),
            stt_provider_key_available: self.stt_provider_key(&file.stt_base_url).is_some(),
        }
    }
}

/// Validate a transcription base URL: `https`, or `http` for a loopback host that is not the
/// daemon itself; no credentials, query or fragment. Returns the URL without a trailing slash.
fn validate_base_url(raw: &str, daemon_port: u16) -> Result<String, VoiceControlError> {
    let bad = |m: &str| VoiceControlError::Invalid(m.to_owned());
    let url = url::Url::parse(raw.trim()).map_err(|_| bad("`stt_base_url` is not a valid URL"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad(
            "`stt_base_url` must not carry credentials, a query or a fragment",
        ));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(bad("`stt_base_url` has no host")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {
            if url.port_or_known_default() == Some(daemon_port) {
                return Err(bad("`stt_base_url` must not point at this daemon"));
            }
        }
        _ => {
            return Err(bad(
                "`stt_base_url` must be https (http only for localhost)",
            ));
        }
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// A `multipart/form-data` body with the audio file, the model and an optional language.
fn multipart_body(
    boundary: &str,
    audio: &[u8],
    mime: &str,
    model: &str,
    language: Option<&str>,
) -> Vec<u8> {
    let ext = match mime.split(';').next().unwrap_or("").trim() {
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        _ => "bin",
    };
    let mut out = Vec::with_capacity(audio.len() + 512);
    let mut field = |name: &str, value: &str| {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    field("model", model);
    field("response_format", "json");
    if let Some(l) = language {
        field("language", l);
    }
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.{ext}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(audio);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl VoiceControl for VoiceHost {
    fn get(&self) -> VoiceConfigDto {
        self.dto(&self.load())
    }

    async fn update(&self, input: VoiceConfigInput) -> Result<VoiceConfigDto, VoiceControlError> {
        if input.clear_gemini_key == Some(true) && input.gemini_key.is_some() {
            return Err(VoiceControlError::Invalid(
                "cannot both set and clear gemini_key in the same request".into(),
            ));
        }
        if input.clear_stt_key == Some(true) && input.stt_key.is_some() {
            return Err(VoiceControlError::Invalid(
                "cannot both set and clear stt_key in the same request".into(),
            ));
        }
        let _guard = self.update_lock.lock().await;
        let mut file = self.load();
        if let Some(u) = &input.stt_base_url {
            file.stt_base_url = if u.trim().is_empty() {
                default_base_url()
            } else {
                validate_base_url(u, self.daemon_port)?
            };
        }
        if let Some(m) = &input.stt_model {
            let m = m.trim();
            if m.chars().count() > MAX_FIELD_CHARS || m.contains(['\r', '\n', '"']) {
                return Err(VoiceControlError::Invalid(
                    "`stt_model` is not a valid model name".into(),
                ));
            }
            file.stt_model = if m.is_empty() {
                default_model()
            } else {
                m.to_owned()
            };
        }
        if let Some(p) = input.stt_provider {
            file.stt_provider = p;
        }
        if let Some(u) = input.use_provider_key {
            file.use_provider_key = u;
        }
        if let Some(m) = &input.gemini_model {
            let m = m.trim();
            if m.chars().count() > 100
                || !m
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            {
                return Err(VoiceControlError::Invalid(
                    "`gemini_model` is not a valid model name".into(),
                ));
            }
            file.gemini_model = if m.is_empty() {
                default_gemini_model()
            } else {
                m.to_owned()
            };
        }
        if let Some(m) = &input.local_model {
            let m = m.trim();
            if crate::voice_local::find_model(m).is_none() {
                return Err(VoiceControlError::Invalid(
                    "`local_model` is not one of the offered speech models".into(),
                ));
            }
            m.clone_into(&mut file.local_model);
        }
        let store = self.secrets.as_ref().map(Arc::clone);
        if let Some(key) = input.gemini_key {
            let Some(store) = store.clone() else {
                return Err(VoiceControlError::Invalid(
                    "secret store is not available to store the key".into(),
                ));
            };
            tokio::task::spawn_blocking(move || store.set(GEMINI_KEY_NAME, &key))
                .await
                .map_err(|e| VoiceControlError::Internal(format!("secret task failed: {e}")))?
                .map_err(|e| VoiceControlError::Internal(format!("cannot store secret: {e}")))?;
        } else if input.clear_gemini_key == Some(true)
            && let Some(store) = store.clone()
        {
            tokio::task::spawn_blocking(move || store.delete(GEMINI_KEY_NAME))
                .await
                .map_err(|e| VoiceControlError::Internal(format!("secret task failed: {e}")))?
                .map_err(|e| VoiceControlError::Internal(format!("cannot delete secret: {e}")))?;
        }
        if let Some(key) = input.stt_key {
            let Some(store) = store else {
                return Err(VoiceControlError::Invalid(
                    "secret store is not available to store the key".into(),
                ));
            };
            tokio::task::spawn_blocking(move || store.set(KEY_NAME, &key))
                .await
                .map_err(|e| VoiceControlError::Internal(format!("secret task failed: {e}")))?
                .map_err(|e| VoiceControlError::Internal(format!("cannot store secret: {e}")))?;
        } else if input.clear_stt_key == Some(true)
            && let Some(store) = store
        {
            tokio::task::spawn_blocking(move || {
                let _ = store.delete(KEY_NAME);
            })
            .await
            .map_err(|e| VoiceControlError::Internal(format!("secret task failed: {e}")))?;
        }
        self.save(&file)?;
        Ok(self.dto(&file))
    }

    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: String,
        language: Option<String>,
        local_only: bool,
    ) -> Result<TranscribeOutput, VoiceControlError> {
        let file = self.load();
        if local_only && file.stt_provider != VoiceProvider::LocalVoice {
            return Err(VoiceControlError::NotConfigured);
        }
        if file.stt_provider == VoiceProvider::LocalVoice {
            return self
                .transcribe_local(audio, &mime, language, &file.local_model)
                .await;
        }
        if file.stt_provider == VoiceProvider::GeminiLive {
            let key = self
                .effective_gemini_key(&file)
                .ok_or(VoiceControlError::NotConfigured)?;
            let base = crate::voice_gemini::validate_ws_base(&self.gemini_ws_base)?;
            if mime.split(';').next().map(str::trim) != Some("audio/pcm") {
                return Err(VoiceControlError::Invalid(
                    "Gemini Live needs `audio/pcm` (16-bit little-endian, 16 kHz, mono)".into(),
                ));
            }
            if audio.len() > MAX_GEMINI_PCM_BYTES {
                return Err(VoiceControlError::Invalid(
                    "the recording is longer than 120 seconds".into(),
                ));
            }
            let text = crate::voice_gemini::transcribe_pcm(&base, &key, &file.gemini_model, &audio)
                .await?;
            return Ok(TranscribeOutput { text });
        }
        let local = validate_base_url(&file.stt_base_url, self.daemon_port)?;
        let key = self.effective_stt_key(&file);
        // A cloud service needs a key; a loopback server may run without one.
        if key.is_none() && local.starts_with("https://") {
            return Err(VoiceControlError::NotConfigured);
        }
        let language = language
            .filter(|l| (2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase()));
        let boundary = format!("praxi-{:016x}", rand_u64());
        let body = multipart_body(
            &boundary,
            &audio,
            &mime,
            &file.stt_model,
            language.as_deref(),
        );
        let mut req = self
            .http
            .post(format!("{local}/audio/transcriptions"))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        if let Some(k) = key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| {
            VoiceControlError::Upstream(format!(
                "transcription request failed: {}",
                e.without_url()
            ))
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VoiceControlError::Upstream(format!(
                "the transcription service answered {status}"
            )));
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| {
            VoiceControlError::Upstream("unreadable answer from the transcription service".into())
        })?;
        let text = v
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| VoiceControlError::Upstream("the answer had no text".into()))?;
        Ok(TranscribeOutput {
            text: text.trim().to_owned(),
        })
    }

    fn local_status(&self) -> LocalVoiceStatusDto {
        self.local.status()
    }

    #[allow(clippy::unused_async)]
    async fn local_install(&self, model: String) -> Result<LocalVoiceStatusDto, VoiceControlError> {
        self.local
            .start_install(model.trim())
            .map_err(VoiceControlError::Invalid)?;
        Ok(self.local.status())
    }
}

impl VoiceHost {
    /// On-device transcription: audio is decoded, cut by Silero VAD and read by Whisper in this
    /// process. Nothing is sent anywhere.
    #[cfg(feature = "local-voice")]
    async fn transcribe_local(
        &self,
        audio: Vec<u8>,
        mime: &str,
        language: Option<String>,
        model: &str,
    ) -> Result<TranscribeOutput, VoiceControlError> {
        if !self.local.ready(model) {
            return Err(VoiceControlError::Invalid(
                "the speech model is not downloaded yet: install it in Settings > Dictation".into(),
            ));
        }
        let language = language
            .filter(|l| (2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase()));
        let (mime, model) = (mime.to_owned(), model.to_owned());
        let engine = Arc::clone(&self.engine);
        let dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let samples = crate::voice_local::decode_audio(&audio, &mime)?;
            engine.transcribe(&dir, &model, language.as_deref(), &samples)
        })
        .await
        .map_err(|e| VoiceControlError::Internal(format!("speech task failed: {e}")))?
        .map(|text| TranscribeOutput { text })
        .map_err(VoiceControlError::Invalid)
    }

    #[cfg(not(feature = "local-voice"))]
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn transcribe_local(
        &self,
        _audio: Vec<u8>,
        _mime: &str,
        _language: Option<String>,
        _model: &str,
    ) -> Result<TranscribeOutput, VoiceControlError> {
        Err(VoiceControlError::Invalid(
            "this build of Praxi-Bot does not include on-device voice".into(),
        ))
    }
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert_eq!(
            validate_base_url("https://api.openai.com/v1/", 7000).unwrap(),
            "https://api.openai.com/v1"
        );
        assert!(validate_base_url("http://127.0.0.1:8080/v1", 7000).is_ok());
        assert!(validate_base_url("http://127.0.0.1:7000/v1", 7000).is_err());
        assert!(validate_base_url("http://example.com/v1", 7000).is_err());
        assert!(validate_base_url("https://user:pw@example.com/v1", 7000).is_err());
    }

    #[test]
    fn multipart_carries_model_language_and_file() {
        let body = multipart_body(
            "B",
            b"AUDIO",
            "audio/webm;codecs=opus",
            "whisper-1",
            Some("tr"),
        );
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("name=\"model\"\r\n\r\nwhisper-1\r\n"));
        assert!(s.contains("name=\"language\"\r\n\r\ntr\r\n"));
        assert!(s.contains("filename=\"audio.webm\""));
        assert!(s.contains("AUDIO\r\n--B--"));
    }

    /// A store whose backend refuses every delete.
    struct StuckStore;

    impl SecretStore for StuckStore {
        fn get(
            &self,
            _: &str,
        ) -> Result<Option<secret_store::SecretValue>, secret_store::SecretError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), secret_store::SecretError> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<bool, secret_store::SecretError> {
            Err(secret_store::SecretError::NotFound("locked".into()))
        }
        fn list_names(&self) -> Result<Vec<String>, secret_store::SecretError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn clearing_the_gemini_key_surfaces_a_store_failure() {
        let dir = std::env::temp_dir().join(format!("praxi-voice-clear-{}", rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = VoiceHost::new(&dir, 7000, Some(Arc::new(StuckStore)));
        let err = host
            .update(VoiceConfigInput {
                clear_gemini_key: Some(true),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, VoiceControlError::Internal(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn local_voice_validates_model_and_never_uses_the_network() {
        let dir = std::env::temp_dir().join(format!("praxi-voice-local-{}", rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = VoiceHost::new(&dir, 7000, None);
        let bad = host
            .update(VoiceConfigInput {
                local_model: Some("../../etc/passwd".into()),
                ..Default::default()
            })
            .await;
        assert!(matches!(bad, Err(VoiceControlError::Invalid(_))));
        let dto = host
            .update(VoiceConfigInput {
                stt_provider: Some(VoiceProvider::LocalVoice),
                local_model: Some("whisper-base".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(dto.local_model, "whisper-base");
        // Nothing is installed: an honest, actionable error instead of a silent fallback to a
        // cloud service (the audio must never leave the machine in this mode).
        let err = host
            .transcribe(vec![0; 64], "audio/pcm".into(), None, false)
            .await
            .unwrap_err();
        let VoiceControlError::Invalid(m) = err else {
            panic!("{err:?}");
        };
        assert!(
            m.contains("not download") || m.contains("does not include"),
            "{m}"
        );
        let st = host.local_status();
        assert!(st.models.iter().all(|m| !m.installed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn host_with_provider_keys(tag: &str) -> (VoiceHost, PathBuf) {
        let dir = std::env::temp_dir().join(format!("praxi-voice-{tag}-{}", rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = secret_store::MemoryStore::new();
        store
            .set("provider.gemini.api_key", "PROVIDER-GEMINI-SECRET")
            .unwrap();
        store
            .set("provider.mine.api_key", "PROVIDER-WHISPER-SECRET")
            .unwrap();
        let mut cfg = ProviderConfig::default();
        let mut g = ProviderSettings::new("gemini", 1);
        g.key = Some(secret_store::SecretRef::new("provider.gemini.api_key").unwrap());
        cfg.upsert(g);
        let mut m = ProviderSettings::new("mine", 2);
        m.key = Some(secret_store::SecretRef::new("provider.mine.api_key").unwrap());
        m.custom = Some(
            toml::from_str(
                "id = 'mine'\nkind = 'open_ai_compatible'\nbase_url = 'https://stt.example.com/v1'\n\
                 auth = { ApiKeyEnv = 'X' }\n[capabilities]\ntools = true\nvision = false\n\
                 streaming = true\njson_mode = false\n[compat_flags]\n",
            )
            .unwrap(),
        );
        cfg.upsert(m);
        cfg.save(&config_path(&dir)).unwrap();
        (VoiceHost::new(&dir, 7000, Some(Arc::new(store))), dir)
    }

    #[tokio::test]
    async fn voice_reuses_the_provider_key_without_exposing_it() {
        let (host, dir) = host_with_provider_keys("reuse");
        let dto = host.get();
        assert!(dto.use_provider_key, "reuse is on by default");
        assert!(dto.gemini_provider_key_available);
        assert!(!dto.gemini_key_set, "no separate voice key was stored");
        let file = host.load();
        assert_eq!(
            host.effective_gemini_key(&file).as_deref(),
            Some("PROVIDER-GEMINI-SECRET")
        );
        // Nothing the API returns or logs carries the key.
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("SECRET"), "{json}");
        // A separate voice key is used once the owner turns reuse off.
        host.update(VoiceConfigInput {
            gemini_key: Some("OWN-VOICE-KEY".into()),
            use_provider_key: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(
            host.effective_gemini_key(&host.load()).as_deref(),
            Some("OWN-VOICE-KEY")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_provider_key_is_only_offered_to_its_own_host() {
        let (host, dir) = host_with_provider_keys("host");
        // Default dictation URL (OpenAI): the unrelated provider's key must not be used.
        assert!(host.effective_stt_key(&host.load()).is_none());
        assert!(!host.get().stt_provider_key_available);
        let dto = host
            .update(VoiceConfigInput {
                stt_base_url: Some("https://stt.example.com/v1/".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(dto.stt_provider_key_available);
        assert_eq!(
            host.effective_stt_key(&host.load()).as_deref(),
            Some("PROVIDER-WHISPER-SECRET")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn gemini_provider_needs_a_key_and_a_valid_model() {
        let dir = std::env::temp_dir().join(format!("praxi-voice-test-{}", rand_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = VoiceHost::new(&dir, 7000, None);
        let dto = host
            .update(VoiceConfigInput {
                stt_provider: Some(VoiceProvider::GeminiLive),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(dto.stt_provider, VoiceProvider::GeminiLive);
        assert!(!dto.gemini_key_set);
        let err = host
            .transcribe(vec![0; 64], "audio/pcm".into(), None, false)
            .await
            .unwrap_err();
        assert_eq!(err, VoiceControlError::NotConfigured);
        let bad = host
            .update(VoiceConfigInput {
                gemini_model: Some("x y".into()),
                ..Default::default()
            })
            .await;
        assert!(matches!(bad, Err(VoiceControlError::Invalid(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
