//! On-device speech-to-text (`stt_provider = "local_voice"`): the model catalog, the checksummed
//! on-demand download into `<data_dir>/models/voice/`, and audio decoding. The recogniser itself
//! (Silero VAD + Whisper through sherpa-onnx) lives in [`crate::voice_local_engine`] behind the
//! `local-voice` cargo feature; everything here compiles and is tested without it.
//!
//! Safety properties: downloads are https-only, pinned to a revision, limited to a short host
//! allow-list (also across redirects), capped at the size the catalog declares and accepted only
//! when the sha256 matches. A bad download is deleted, never used.
//!
//! Model licences (shown to the owner before download): Whisper weights MIT (`OpenAI`), Silero VAD
//! MIT (Silero Team). The ONNX conversions are published by the `sherpa-onnx` project (Apache-2.0).
//! No Piper (GPL) and no Parakeet (CC-BY-4.0, would need attribution) are offered.

// The audio decoding half is only reached through the recogniser (feature `local-voice`) but is
// compiled and unit-tested in every build.
#![cfg_attr(not(feature = "local-voice"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use daemon_api::voice::{LocalVoiceModelDto, LocalVoiceStatusDto};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

/// Sample rate the recogniser and the VAD expect.
pub const SAMPLE_RATE: u32 = 16_000;
/// Longest accepted recording: 5 minutes at 16 kHz.
pub const MAX_SAMPLES: usize = 5 * 60 * SAMPLE_RATE as usize;

/// One file of a model: its name on disk, where it comes from, its exact size and sha256.
#[derive(Debug, Clone, Copy)]
pub struct FileSpec {
    pub name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// A downloadable speech model.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub files: &'static [FileSpec],
}

/// Voice-activity model (end of speech), shared by every speech model.
pub const VAD: ModelSpec = ModelSpec {
    id: "silero-vad",
    label: "Silero VAD",
    description: "Finds where speech starts and ends.",
    license: "MIT",
    license_url: "https://github.com/snakers4/silero-vad/blob/master/LICENSE",
    files: &[FileSpec {
        name: "silero_vad.onnx",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
        bytes: 643_854,
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    }],
};

const WHISPER_LICENSE_URL: &str = "https://github.com/openai/whisper/blob/main/LICENSE";

/// Whisper models (multilingual, int8). Files are pinned to a Hugging Face revision.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "whisper-tiny",
        label: "Whisper tiny (fast, rough)",
        description: "Smallest and fastest. Fine for short English notes; weak for Turkish and German.",
        license: "MIT",
        license_url: WHISPER_LICENSE_URL,
        files: &[
            FileSpec {
                name: "encoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-tiny/resolve/65176e2deb88badc814a94058666cadccc29b61c/tiny-encoder.int8.onnx",
                bytes: 12_937_772,
                sha256: "d24fb083ae3b1041fc24e97971d60e280c9342201fbb67b0ab428a8b4a51a434",
            },
            FileSpec {
                name: "decoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-tiny/resolve/65176e2deb88badc814a94058666cadccc29b61c/tiny-decoder.int8.onnx",
                bytes: 89_855_401,
                sha256: "d2fece8dd42771f1df975c6c0445770d0c292bf7547c2cae04a6c0cc57540925",
            },
            FileSpec {
                name: "tokens.txt",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-tiny/resolve/65176e2deb88badc814a94058666cadccc29b61c/tiny-tokens.txt",
                bytes: 816_730,
                sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
            },
        ],
    },
    ModelSpec {
        id: "whisper-base",
        label: "Whisper base (balanced)",
        description: "Good speed, understands many languages including Turkish and German.",
        license: "MIT",
        license_url: WHISPER_LICENSE_URL,
        files: &[
            FileSpec {
                name: "encoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-encoder.int8.onnx",
                bytes: 29_120_534,
                sha256: "0b8fb1304b6109976038efff5ace81720e00386f3ff6b54ee8c75291ca0a1e11",
            },
            FileSpec {
                name: "decoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-decoder.int8.onnx",
                bytes: 130_672_026,
                sha256: "9759d217388a01b3a4c7c15533201067b48ae819c4daafc8624e64b9409dc02d",
            },
            FileSpec {
                name: "tokens.txt",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-tokens.txt",
                bytes: 816_730,
                sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
            },
        ],
    },
    ModelSpec {
        id: "whisper-small",
        label: "Whisper small (most accurate)",
        description: "Best accuracy here, also for Turkish and German. Larger download, a bit slower.",
        license: "MIT",
        license_url: WHISPER_LICENSE_URL,
        files: &[
            FileSpec {
                name: "encoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-encoder.int8.onnx",
                bytes: 112_442_483,
                sha256: "4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9",
            },
            FileSpec {
                name: "decoder.int8.onnx",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-decoder.int8.onnx",
                bytes: 262_226_114,
                sha256: "acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee",
            },
            FileSpec {
                name: "tokens.txt",
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-tokens.txt",
                bytes: 816_730,
                sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
            },
        ],
    },
];

/// Model used when the owner has not picked one.
pub const DEFAULT_MODEL: &str = "whisper-small";

/// Catalog entry by id.
#[must_use]
pub fn find_model(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// `<data_dir>/models/voice`.
#[must_use]
pub fn models_root(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("voice")
}

fn model_dir(data_dir: &Path, spec: &ModelSpec) -> PathBuf {
    models_root(data_dir).join(spec.id)
}

fn size_mb(spec: &ModelSpec) -> u32 {
    let total: u64 = spec.files.iter().map(|f| f.bytes).sum();
    u32::try_from(total.div_ceil(1_000_000)).unwrap_or(u32::MAX)
}

/// Every file present with the expected size AND sha256 (re-hashed: size alone is not trusted).
#[must_use]
pub fn is_installed(data_dir: &Path, spec: &ModelSpec) -> bool {
    let dir = model_dir(data_dir, spec);
    spec.files
        .iter()
        .all(|f| file_matches(&dir.join(f.name), f))
}

fn file_matches(path: &Path, f: &FileSpec) -> bool {
    use std::io::Read as _;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == f.bytes) {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    hex(&hasher.finalize()) == f.sha256
}

/// Where a file of an installed model lives.
#[cfg(feature = "local-voice")]
#[must_use]
pub fn file_path(data_dir: &Path, spec: &ModelSpec, name: &str) -> PathBuf {
    model_dir(data_dir, spec).join(name)
}

/// `https` and a host the catalog may point at. Applied to the first URL and to every redirect.
#[must_use]
pub fn is_allowed_download_url(url: &url::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let under = |root: &str| host == root || host.ends_with(&format!(".{root}"));
    under("huggingface.co")
        || under("hf.co")
        || under("github.com")
        || under("githubusercontent.com")
}

/// Download state shared between the installer task and `GET /v1/voice/local`.
#[derive(Default)]
struct InstallState {
    installing: Option<String>,
    last_error: Option<String>,
}

/// Catalog, installer and (with the feature) the recogniser.
pub struct LocalVoice {
    data_dir: PathBuf,
    state: Arc<Mutex<InstallState>>,
    http: reqwest::Client,
}

impl LocalVoice {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !is_allowed_download_url(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            data_dir: data_dir.to_path_buf(),
            state: Arc::new(Mutex::new(InstallState::default())),
            http,
        }
    }

    /// Whether this build can run the recogniser at all.
    #[must_use]
    pub const fn compiled_in() -> bool {
        cfg!(feature = "local-voice")
    }

    /// Catalog with install state.
    #[must_use]
    pub fn status(&self) -> LocalVoiceStatusDto {
        let st = self.state.lock();
        LocalVoiceStatusDto {
            available: Self::compiled_in(),
            vad_installed: is_installed(&self.data_dir, &VAD),
            models: MODELS
                .iter()
                .map(|m| LocalVoiceModelDto {
                    id: m.id.to_owned(),
                    label: m.label.to_owned(),
                    description: m.description.to_owned(),
                    size_mb: size_mb(m),
                    license: m.license.to_owned(),
                    license_url: m.license_url.to_owned(),
                    source_host: m
                        .files
                        .first()
                        .and_then(|f| url::Url::parse(f.url).ok())
                        .and_then(|u| u.host_str().map(str::to_owned))
                        .unwrap_or_default(),
                    installed: is_installed(&self.data_dir, m),
                })
                .collect(),
            installing: st.installing.clone(),
            last_error: st.last_error.clone(),
        }
    }

    /// Both the VAD and `model` are on disk.
    #[cfg(feature = "local-voice")]
    #[must_use]
    pub fn ready(&self, model: &str) -> bool {
        find_model(model).is_some_and(|m| is_installed(&self.data_dir, m))
            && is_installed(&self.data_dir, &VAD)
    }

    /// Start downloading `model` (and the VAD if missing) in the background.
    ///
    /// # Errors
    /// A plain-language reason: unknown model, build without the feature, or a download already running.
    pub fn start_install(&self, model: &str) -> Result<(), String> {
        if !Self::compiled_in() {
            return Err("this build of Praxi-Bot does not include on-device voice".into());
        }
        let spec = find_model(model).ok_or_else(|| "unknown speech model".to_owned())?;
        {
            let mut st = self.state.lock();
            if st.installing.is_some() {
                return Err("another speech model is already downloading".into());
            }
            st.installing = Some(spec.id.to_owned());
            st.last_error = None;
        }
        let state = Arc::clone(&self.state);
        let http = self.http.clone();
        let dir = self.data_dir.clone();
        tokio::spawn(async move {
            let mut result = Ok(());
            for m in [&VAD, spec] {
                if !is_installed(&dir, m) {
                    result = download_model(&http, &dir, m).await;
                    if result.is_err() {
                        break;
                    }
                }
            }
            let mut st = state.lock();
            st.installing = None;
            st.last_error = result.err();
        });
        Ok(())
    }
}

async fn download_model(
    http: &reqwest::Client,
    data_dir: &Path,
    spec: &ModelSpec,
) -> Result<(), String> {
    let dir = model_dir(data_dir, spec);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create the model folder: {e}"))?;
    for f in spec.files {
        if file_matches(&dir.join(f.name), f) {
            continue;
        }
        let free = fs4::available_space(&dir).unwrap_or(u64::MAX);
        if free < f.bytes.saturating_add(256 * 1024 * 1024) {
            return Err("not enough free disk space for the speech model".into());
        }
        download_file(http, f, &dir.join(f.name)).await?;
    }
    Ok(())
}

async fn download_file(http: &reqwest::Client, f: &FileSpec, dest: &Path) -> Result<(), String> {
    use std::io::Write as _;
    let url = url::Url::parse(f.url).map_err(|_| "bad download address".to_owned())?;
    if !is_allowed_download_url(&url) {
        return Err("the download address is not allowed".into());
    }
    let part = dest.with_extension("part");
    let outcome = async {
        let mut resp = http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("the download failed: {}", e.without_url()))?;
        if !resp.status().is_success() {
            return Err(format!("the download server answered {}", resp.status()));
        }
        let mut out =
            std::fs::File::create(&part).map_err(|e| format!("cannot write the model: {e}"))?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("the download was interrupted: {}", e.without_url()))?
        {
            total += chunk.len() as u64;
            if total > f.bytes {
                return Err("the download is larger than expected and was discarded".into());
            }
            hasher.update(&chunk);
            out.write_all(&chunk)
                .map_err(|e| format!("cannot write the model (disk full?): {e}"))?;
        }
        out.flush()
            .map_err(|e| format!("cannot write the model: {e}"))?;
        drop(out);
        if total != f.bytes || hex(&hasher.finalize()) != f.sha256 {
            return Err(format!(
                "{} failed its checksum and was discarded; try again later",
                f.name
            ));
        }
        std::fs::rename(&part, dest).map_err(|e| format!("cannot save the model: {e}"))
    }
    .await;
    if outcome.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    outcome
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// ---------------------------------------------------------------------------------------------
// Audio decoding: whatever the caller sends -> mono f32 at 16 kHz.
// ---------------------------------------------------------------------------------------------

/// Decode a recording into mono `f32` samples at 16 kHz.
///
/// Accepts `audio/pcm` (16-bit LE, 16 kHz, mono), `audio/wav` (16-bit PCM, any rate, mono or
/// stereo) and, with the `local-voice` feature, `audio/ogg` (Opus: Telegram voice notes).
///
/// # Errors
/// A plain-language reason.
pub fn decode_audio(audio: &[u8], mime: &str) -> Result<Vec<f32>, String> {
    let kind = mime.split(';').next().unwrap_or("").trim();
    let samples = match kind {
        "audio/pcm" => {
            if !audio.len().is_multiple_of(2) {
                return Err("the audio is not 16-bit PCM".into());
            }
            pcm16_to_f32(audio)
        }
        "audio/wav" | "audio/x-wav" | "audio/wave" => decode_wav(audio)?,
        "audio/ogg" | "audio/opus" => decode_ogg(audio)?,
        _ => {
            return Err(
                "on-device voice reads audio/pcm, audio/wav or audio/ogg (Opus) recordings".into(),
            );
        }
    };
    if samples.len() > MAX_SAMPLES {
        return Err("the recording is longer than 5 minutes".into());
    }
    Ok(samples)
}

#[cfg(feature = "local-voice")]
fn decode_ogg(audio: &[u8]) -> Result<Vec<f32>, String> {
    crate::voice_local_engine::decode_ogg_opus(audio)
}

#[cfg(not(feature = "local-voice"))]
fn decode_ogg(_audio: &[u8]) -> Result<Vec<f32>, String> {
    Err("this build of Praxi-Bot does not include on-device voice".into())
}

fn pcm16_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| f32::from(i16::from_le_bytes(*c)) / 32_768.0)
        .collect()
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

fn decode_wav(b: &[u8]) -> Result<Vec<f32>, String> {
    let bad = || "the WAV file is damaged or not 16-bit PCM".to_owned();
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad());
    }
    let (mut pos, mut fmt, mut data) = (12usize, None, None);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = le32(b, pos + 4).ok_or_else(bad)? as usize;
        let body = pos + 8;
        let end = body.saturating_add(len).min(b.len());
        if id == b"fmt " {
            fmt = Some((
                le16(b, body).ok_or_else(bad)?,
                le16(b, body + 2).ok_or_else(bad)?,
                le32(b, body + 4).ok_or_else(bad)?,
                le16(b, body + 14).ok_or_else(bad)?,
            ));
        } else if id == b"data" {
            data = Some(&b[body..end]);
            break;
        }
        pos = body.saturating_add(len).saturating_add(len & 1);
    }
    let ((format, channels, rate, bits), data) = fmt.zip(data).ok_or_else(bad)?;
    if format != 1 || bits != 16 || !(1..=2).contains(&channels) || rate == 0 {
        return Err(bad());
    }
    let raw = pcm16_to_f32(&data[..data.len() & !1]);
    let mono: Vec<f32> = if channels == 2 {
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|[l, r]| l.midpoint(*r))
            .collect()
    } else {
        raw
    };
    Ok(resample(&mono, rate))
}

/// Linear resampling to 16 kHz (averaging when decimating by an integer factor).
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn resample(input: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == SAMPLE_RATE || input.is_empty() {
        return input.to_vec();
    }
    if from_rate.is_multiple_of(SAMPLE_RATE) {
        let k = (from_rate / SAMPLE_RATE) as usize;
        return input
            .chunks(k)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect();
    }
    let ratio = f64::from(from_rate) / f64::from(SAMPLE_RATE);
    let n = (input.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = pos as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = input[i0];
            let b = input.get(i0 + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut v = b"RIFF".to_vec();
        v.extend((36 + data.len() as u32).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * u32::from(channels) * 2).to_le_bytes());
        v.extend((channels * 2).to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    }

    #[test]
    fn catalog_is_pinned_and_licensed() {
        for m in MODELS.iter().chain([&VAD]) {
            assert!(!m.license.is_empty() && m.license_url.starts_with("https://"));
            for f in m.files {
                let u = url::Url::parse(f.url).unwrap();
                assert!(is_allowed_download_url(&u), "{}", f.url);
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(f.bytes > 0);
            }
        }
        // Whisper weights are MIT; a copyleft or attribution-only model must not slip in unnoticed.
        assert!(MODELS.iter().all(|m| m.license == "MIT"));
        assert!(find_model(DEFAULT_MODEL).is_some());
    }

    #[test]
    fn downloads_only_from_allowed_https_hosts() {
        let ok = |s: &str| is_allowed_download_url(&url::Url::parse(s).unwrap());
        assert!(ok("https://huggingface.co/a/b"));
        assert!(ok("https://cdn-lfs.hf.co/x"));
        assert!(ok("https://objects.githubusercontent.com/x"));
        assert!(!ok("http://huggingface.co/a"));
        assert!(!ok("https://huggingface.co.evil.example/a"));
        assert!(!ok("https://evilhuggingface.co/a"));
        assert!(!ok("https://user:pw@huggingface.co/a"));
    }

    #[test]
    fn install_state_follows_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let spec = find_model("whisper-tiny").unwrap();
        assert!(!is_installed(dir.path(), spec));
        // Right names, wrong sizes: not installed (a truncated download must not count).
        let d = model_dir(dir.path(), spec);
        std::fs::create_dir_all(&d).unwrap();
        for f in spec.files {
            std::fs::write(d.join(f.name), b"x").unwrap();
        }
        assert!(!is_installed(dir.path(), spec));
        let st = LocalVoice::new(dir.path()).status();
        assert!(st.models.iter().all(|m| !m.installed));
        assert!(st.models.iter().any(|m| m.id == DEFAULT_MODEL));
    }

    #[test]
    fn same_size_but_tampered_file_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.bin");
        std::fs::write(&path, b"abcd").unwrap();
        let mut h = Sha256::new();
        h.update(b"abcd");
        let sha = hex(&h.finalize());
        let sha: &'static str = Box::leak(sha.into_boxed_str());
        let spec = FileSpec {
            name: "m.bin",
            url: "https://huggingface.co/x",
            bytes: 4,
            sha256: sha,
        };
        assert!(file_matches(&path, &spec));
        std::fs::write(&path, b"abce").unwrap();
        assert!(!file_matches(&path, &spec));
    }

    #[test]
    fn wav_is_downmixed_and_resampled_to_16k() {
        // 48 kHz stereo, 6 frames -> 2 mono samples at 16 kHz.
        let l = [16_384i16, 16_384, 16_384, 0, 0, 0];
        let r = [16_384i16, 16_384, 16_384, 0, 0, 0];
        let interleaved: Vec<i16> = l.iter().zip(r).flat_map(|(a, b)| [*a, b]).collect();
        let out = decode_audio(&wav(48_000, 2, &interleaved), "audio/wav").unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.5).abs() < 1e-3 && out[1].abs() < 1e-3);
        assert!(decode_audio(b"RIFFxxxxWAVE", "audio/wav").is_err());
    }

    #[test]
    fn pcm_and_unknown_types() {
        let out = decode_audio(&[0, 64, 0, 0xC0], "audio/pcm").unwrap();
        assert!((out[0] - 0.5).abs() < 1e-3 && (out[1] + 0.5).abs() < 1e-3);
        assert!(decode_audio(&[1, 2, 3], "audio/pcm").is_err());
        assert!(decode_audio(&[0; 8], "audio/webm").is_err());
    }
}
