//! On-device voice: the model catalog, the checksummed
//! resumable on-demand download into `<data_dir>/models/voice/`, and audio formats. Inference
//! (Silero VAD, Whisper and Supertonic through sherpa-onnx) runs in the voice worker behind the
//! `local-voice` cargo feature. The catalog, installer and PCM helpers can be tested without it.
//!
//! Safety properties: downloads are https-only, pinned to a revision, limited to a short host
//! allow-list (also across redirects), capped at the size the catalog declares and accepted only
//! when the sha256 matches. A bad download is deleted, never used.
//!
//! Model licences (shown to the owner before download): Whisper weights MIT (`OpenAI`), Silero VAD
//! MIT (Silero Team) and Supertonic 3 weights OpenRAIL-M (Supertone). The ONNX conversions
//! are published by the `sherpa-onnx` project (Apache-2.0).

// The audio decoding half is only reached through the recogniser (feature `local-voice`) but is
// compiled and unit-tested in every build.
#![cfg_attr(not(feature = "local-voice"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use daemon_api::voice::{LocalVoiceModelDto, LocalVoiceStatusDto};
use parking_lot::Mutex;
use provider_catalog::local_runtime::download::{
    DownloadSpec, check_disk_space, download_verified, missing_bytes, sha256_file,
};

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

/// Sherpa's int8 conversion of Supertonic 3, including its fixed speaker styles.
pub const TTS: ModelSpec = ModelSpec {
    id: "supertonic-3",
    label: "Supertonic 3",
    description: "CPU speech synthesis in English, Turkish, German and other languages.",
    license: "OpenRAIL-M",
    license_url: "https://huggingface.co/Supertone/supertonic-3/blob/724fb5abbf5502583fb520898d45929e62f02c0b/LICENSE",
    files: &[
        FileSpec {
            name: "duration_predictor.int8.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/duration_predictor.int8.onnx",
            bytes: 3_700_147,
            sha256: "c3eb91414d5ff8a7a239b7fe9e34e7e2bf8a8140d8375ffb14718b1c639325db",
        },
        FileSpec {
            name: "text_encoder.int8.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/text_encoder.int8.onnx",
            bytes: 36_416_150,
            sha256: "c7befd5ea8c3119769e8a6c1486c4edc6a3bc8365c67621c881bbb774b9902ff",
        },
        FileSpec {
            name: "vector_estimator.int8.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/vector_estimator.int8.onnx",
            bytes: 78_400_833,
            sha256: "20cd86fa5c6effedfda0e7cffe5b0569ca401c440a0c3a1d72bf39286c0db3fd",
        },
        FileSpec {
            name: "vocoder.int8.onnx",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/vocoder.int8.onnx",
            bytes: 25_991_073,
            sha256: "e923d60f53f95eb1ce235f1dc33ec56d9c057823c96fa6f8acf98f32b0da6152",
        },
        FileSpec {
            name: "tts.json",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/tts.json",
            bytes: 8_253,
            sha256: "42078d3aef1cd43ab43021f3c54f47d2d75ceb4e75f627f118890128b06a0d09",
        },
        FileSpec {
            name: "unicode_indexer.bin",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/unicode_indexer.bin",
            bytes: 262_144,
            sha256: "8402ca48e5189a8950138580b0fff64db6f072f24ac07cd54ba8b2fbb9883b30",
        },
        FileSpec {
            name: "voice.bin",
            url: "https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/resolve/cca5a0e6c96e1d2c720986bf7e75fcc81dee3ae4/voice.bin",
            bytes: 517_168,
            sha256: "67d5209b0ee8ce6c74105ffbe12fe6a7628aea3b4ba2fcb308a4a67938a93ce8",
        },
    ],
};

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

pub fn stt_ready(data_dir: &Path, model: &str) -> bool {
    find_model(model).is_some_and(|spec| is_installed(data_dir, spec))
        && is_installed(data_dir, &VAD)
}

/// A fast presence check for diagnostics and worker admission. The worker verifies the contents.
pub fn model_files_present(data_dir: &Path, spec: &ModelSpec) -> bool {
    let dir = model_dir(data_dir, spec);
    spec.files
        .iter()
        .all(|file| file_has_size(&dir.join(file.name), file.bytes))
}

pub fn stt_files_present(data_dir: &Path, model: &str) -> bool {
    find_model(model).is_some_and(|spec| model_files_present(data_dir, spec))
        && model_files_present(data_dir, &VAD)
}

pub fn synthesis_language(tag: &str) -> &'static str {
    const LANGUAGES: &[&str] = &[
        "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr",
        "hu", "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk",
        "vi",
    ];
    let base = tag.split('-').next().unwrap_or_default();
    LANGUAGES
        .iter()
        .copied()
        .find(|lang| lang.eq_ignore_ascii_case(base))
        .unwrap_or("en")
}

fn file_matches(path: &Path, f: &FileSpec) -> bool {
    file_has_size(path, f.bytes) && sha256_file(path).is_ok_and(|sha| sha == f.sha256)
}

fn file_has_size(path: &Path, bytes: u64) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes)
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
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
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
#[derive(Clone)]
pub struct LocalVoice {
    data_dir: PathBuf,
    state: Arc<Mutex<InstallState>>,
}

impl LocalVoice {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            state: Arc::new(Mutex::new(InstallState::default())),
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
        let (installing, last_error) = {
            let st = self.state.lock();
            (st.installing.clone(), st.last_error.clone())
        };
        LocalVoiceStatusDto {
            available: Self::compiled_in(),
            vad_installed: is_installed(&self.data_dir, &VAD),
            models: MODELS.iter().map(|m| self.model_status(m)).collect(),
            tts_model: Self::compiled_in().then(|| self.model_status(&TTS)),
            installing,
            last_error,
        }
    }

    fn model_status(&self, spec: &ModelSpec) -> LocalVoiceModelDto {
        let size_mb = if find_model(spec.id).is_some() {
            size_mb_with_vad(spec)
        } else {
            size_mb(spec)
        };
        LocalVoiceModelDto {
            id: spec.id.to_owned(),
            label: spec.label.to_owned(),
            description: spec.description.to_owned(),
            size_mb,
            license: spec.license.to_owned(),
            license_url: spec.license_url.to_owned(),
            source_host: spec
                .files
                .first()
                .and_then(|f| url::Url::parse(f.url).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
            installed: is_installed(&self.data_dir, spec),
        }
    }

    /// Start downloading `model` (and the VAD if missing) in the background.
    ///
    /// # Errors
    /// A plain-language reason: unknown model, build without the feature, or a download already running.
    pub fn start_install(&self, model: &str) -> Result<(), String> {
        if !Self::compiled_in() {
            return Err("this build of Praxi-Bot does not include on-device voice".into());
        }
        let models = if model == TTS.id {
            vec![&TTS]
        } else {
            vec![&VAD, find_model(model).ok_or("unknown speech model")?]
        };
        {
            let mut st = self.state.lock();
            if st.installing.is_some() {
                return Err("another speech model is already downloading".into());
            }
            st.installing = Some(model.to_owned());
            st.last_error = None;
        }
        let state = Arc::clone(&self.state);
        let dir = self.data_dir.clone();
        tokio::spawn(async move {
            let result = install_models(&dir, &models).await;
            let mut st = state.lock();
            st.installing = None;
            st.last_error = result.err();
        });
        Ok(())
    }
}

fn size_mb_with_vad(spec: &ModelSpec) -> u32 {
    let bytes = spec
        .files
        .iter()
        .chain(VAD.files)
        .map(|f| f.bytes)
        .sum::<u64>();
    u32::try_from(bytes.div_ceil(1_000_000)).unwrap_or(u32::MAX)
}

async fn install_models(data_dir: &Path, models: &[&ModelSpec]) -> Result<(), String> {
    let root = models_root(data_dir);
    let files = models
        .iter()
        .flat_map(|model| {
            model
                .files
                .iter()
                .map(|file| (file, model_dir(data_dir, model).join(file.name)))
        })
        .collect::<Vec<_>>();
    let (files, bytes) = tokio::task::spawn_blocking(move || {
        let files = files
            .into_iter()
            .filter(|(file, dest)| !file_matches(dest, file))
            .collect::<Vec<_>>();
        let bytes = files
            .iter()
            .map(|(file, dest)| missing_bytes(dest, file.bytes))
            .sum();
        (files, bytes)
    })
    .await
    .map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| e.to_string())?;
    check_disk_space(&root, bytes).map_err(|e| e.to_string())?;
    let cancel = AtomicBool::new(false);
    for (file, dest) in files {
        let url = url::Url::parse(file.url).map_err(|_| "bad download address")?;
        if !is_allowed_download_url(&url) {
            return Err("the download address is not allowed".into());
        }
        let builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !is_allowed_download_url(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }));
        let spec = DownloadSpec {
            url: file.url,
            dest: &dest,
            size_bytes: file.bytes,
            sha256: file.sha256,
        };
        download_verified(builder, &spec, &|_| {}, &cancel)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
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

/// Encode a mono inference result as a 16-bit PCM WAV for desktop playback.
#[cfg(any(feature = "local-voice", test))]
#[allow(clippy::cast_possible_truncation)] // values are clamped to the i16 range
pub fn encode_wav(rate: u32, samples: &[f32]) -> Result<Vec<u8>, String> {
    if rate == 0 || rate > 192_000 || samples.is_empty() || samples.len() > 8_000_000 {
        return Err("the voice worker generated invalid audio".into());
    }
    let size = u32::try_from(samples.len() * 2).map_err(|_| "the generated audio is too large")?;
    let mut wav = Vec::with_capacity(44 + size as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + size).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes()); // bytes per frame
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(size.to_le_bytes());
    for sample in samples {
        let pcm = (sample * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
        wav.extend(pcm.to_le_bytes());
    }
    Ok(wav)
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
    let frames = data.len() / (2 * usize::from(channels));
    if (frames as u64 * u64::from(SAMPLE_RATE)).div_ceil(u64::from(rate)) > MAX_SAMPLES as u64 {
        return Err("the recording is longer than 5 minutes".into());
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
        for m in MODELS.iter().chain([&VAD, &TTS]) {
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
        let spec = FileSpec {
            name: "m.bin",
            url: "https://huggingface.co/x",
            bytes: 4,
            sha256: "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589",
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
        assert!(decode_audio(&wav(1, 1, &[0; 301]), "audio/wav").is_err());
    }

    #[test]
    fn pcm_and_unknown_types() {
        let out = decode_audio(&[0, 64, 0, 0xC0], "audio/pcm").unwrap();
        assert!((out[0] - 0.5).abs() < 1e-3 && (out[1] + 0.5).abs() < 1e-3);
        assert!(decode_audio(&[1, 2, 3], "audio/pcm").is_err());
        assert!(decode_audio(&[0; 8], "audio/webm").is_err());
    }

    #[test]
    fn synthesizer_output_is_playable_pcm_at_the_engine_rate() {
        let wav = encode_wav(24_000, &[0.5, -0.5, 2.0]).unwrap();
        assert_eq!(le32(&wav, 24), Some(24_000));
        assert_eq!(le32(&wav, 40), Some(6));
        assert_eq!(&wav[44..], &[0, 64, 0, 192, 255, 127]);
        assert!(encode_wav(0, &[]).is_err());
        assert_eq!(synthesis_language("tr-TR"), "tr");
        assert_eq!(synthesis_language("en-US"), "en");
        assert_eq!(synthesis_language("de"), "de");
        assert_eq!(synthesis_language("zh-Hans"), "en");
    }
}
