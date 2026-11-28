//! The recogniser behind `stt_provider = "local_voice"` (compiled only with the `local-voice`
//! feature): Silero VAD cuts a recording into spoken segments, Whisper reads each one.
//!
//! The VAD/recogniser calls follow the official `sherpa-onnx` Rust examples
//! (`rust-api-examples/examples/silero_vad_remove_silence.rs`, `whisper.rs`),
//! Copyright (c) 2026 Xiaomi Corporation / Yujie Wu, Apache-2.0
//! (<https://github.com/k2-fsa/sherpa-onnx>). Adapted: both stages are chained and the segment length is capped for Whisper's 30 s window.
//! Ogg-Opus decoding (Telegram voice notes) uses the pure-Rust `opus-decoder` and `ogg` crates.

use std::path::Path;

use sherpa_onnx::{
    GenerationConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTts, OfflineTtsConfig,
    OfflineTtsModelConfig, OfflineTtsSupertonicModelConfig, OfflineWhisperModelConfig,
    SileroVadModelConfig, TenVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use crate::voice_local::{self, SAMPLE_RATE};

/// VAD window size the Silero model expects at 16 kHz.
const VAD_WINDOW: usize = 512;

/// Transcribe 16 kHz mono samples in the native worker.
pub fn transcribe(
    data_dir: &Path,
    model_id: &str,
    language: Option<&str>,
    samples: &[f32],
) -> Result<String, String> {
    let spec = voice_local::find_model(model_id).ok_or("unknown speech model")?;
    let path = |spec: &voice_local::ModelSpec, name: &str| {
        voice_local::file_path(data_dir, spec, name)
            .to_string_lossy()
            .into_owned()
    };

    let segments = split_speech(&path(&voice_local::VAD, "silero_vad.onnx"), samples)?;
    if segments.is_empty() {
        return Ok(String::new());
    }

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.whisper = OfflineWhisperModelConfig {
        encoder: Some(path(spec, "encoder.int8.onnx")),
        decoder: Some(path(spec, "decoder.int8.onnx")),
        // Empty = Whisper detects the language itself.
        language: Some(language.unwrap_or("").to_owned()),
        task: Some("transcribe".to_owned()),
        tail_paddings: 0,
        enable_token_timestamps: false,
        enable_segment_timestamps: false,
    };
    config.model_config.tokens = Some(path(spec, "tokens.txt"));
    config.model_config.provider = Some("cpu".to_owned());
    config.model_config.num_threads = 2;
    let rec = OfflineRecognizer::create(&config)
        .ok_or("the speech model could not be loaded; download it again")?;

    let mut parts = Vec::new();
    for seg in &segments {
        let stream = rec.create_stream();
        stream.accept_waveform(SAMPLE_RATE.cast_signed(), seg);
        rec.decode(&stream);
        if let Some(r) = stream.get_result() {
            let t = r.text.trim();
            if !t.is_empty() {
                parts.push(t.to_owned());
            }
        }
    }
    Ok(parts.join(" "))
}

/// Supertonic 3 on CPU, with a bundled-in-the-model fixed speaker preset. The model configuration
/// follows sherpa-onnx's Supertonic Rust example (Copyright (c) 2026 Xiaomi Corporation, Apache-2.0).
pub fn synthesize(data_dir: &Path, text: &str, language: &str) -> Result<Vec<u8>, String> {
    let path = |name| {
        Some(
            voice_local::file_path(data_dir, &voice_local::TTS, name)
                .to_string_lossy()
                .into_owned(),
        )
    };
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            supertonic: OfflineTtsSupertonicModelConfig {
                duration_predictor: path("duration_predictor.int8.onnx"),
                text_encoder: path("text_encoder.int8.onnx"),
                vector_estimator: path("vector_estimator.int8.onnx"),
                vocoder: path("vocoder.int8.onnx"),
                tts_json: path("tts.json"),
                unicode_indexer: path("unicode_indexer.bin"),
                voice_style: path("voice.bin"),
            },
            num_threads: 2,
            provider: Some("cpu".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    let engine = OfflineTts::create(&config).ok_or("the read-aloud model could not be loaded")?;
    let generation = GenerationConfig {
        num_steps: 8,
        extra: Some([("lang".to_owned(), serde_json::json!(language))].into()),
        ..Default::default()
    };
    let audio = engine
        .generate_with_config(text, &generation, None::<fn(&[f32], f32) -> bool>)
        .ok_or("the read-aloud model could not generate speech")?;
    let rate = u32::try_from(audio.sample_rate()).map_err(|_| "invalid audio rate")?;
    voice_local::encode_wav(rate, audio.samples())
}

/// Silero VAD: the spoken parts of `samples`, each at most 25 s (Whisper reads 30 s windows).
fn split_speech(vad_model: &str, samples: &[f32]) -> Result<Vec<Vec<f32>>, String> {
    let silero = SileroVadModelConfig {
        model: Some(vad_model.to_owned()),
        threshold: 0.5,
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        max_speech_duration: 25.0,
        ..SileroVadModelConfig::default()
    };
    let config = VadModelConfig {
        silero_vad: silero,
        ten_vad: TenVadModelConfig::default(),
        sample_rate: SAMPLE_RATE.cast_signed(),
        num_threads: 1,
        provider: Some("cpu".to_owned()),
        debug: false,
    };
    let vad = VoiceActivityDetector::create(&config, 30.0)
        .ok_or("the voice-activity model could not be loaded; download it again")?;
    let mut out = Vec::new();
    let mut drain = |vad: &VoiceActivityDetector| {
        while let Some(seg) = vad.front() {
            out.push(seg.samples().to_vec());
            vad.pop();
        }
    };
    for chunk in samples.chunks(VAD_WINDOW) {
        vad.accept_waveform(chunk);
        drain(&vad);
    }
    vad.flush();
    drain(&vad);
    Ok(out)
}

/// Decode an Ogg-Opus stream (Telegram voice note) into 16 kHz mono samples.
///
/// # Errors
/// A plain-language reason.
pub fn decode_ogg_opus(audio: &[u8]) -> Result<Vec<f32>, String> {
    let bad = || "the voice note could not be read (not Ogg Opus)".to_owned();
    let mut reader = ogg::PacketReader::new(std::io::Cursor::new(audio));
    let head = reader.read_packet().map_err(|_| bad())?.ok_or_else(bad)?;
    if head.data.len() < 19 || &head.data[..8] != b"OpusHead" {
        return Err(bad());
    }
    let channel_count = head.data[9];
    if !(1..=2).contains(&channel_count) {
        return Err(bad());
    }
    let channels = usize::from(channel_count);
    // OpusHead pre-skip (samples at 48 kHz) -> samples at 16 kHz to drop from the start.
    let pre_skip = usize::from(u16::from_le_bytes([head.data[10], head.data[11]])) / 3;
    let tags = reader.read_packet().map_err(|_| bad())?.ok_or_else(bad)?;
    if tags.data.len() < 8 || &tags.data[..8] != b"OpusTags" {
        return Err(bad());
    }
    let mut dec = opus_decoder::OpusDecoder::new(SAMPLE_RATE, channels).map_err(|_| bad())?;
    // 120 ms is the longest Opus frame.
    let mut buf = vec![0.0f32; (SAMPLE_RATE as usize * 120 / 1000) * channels];
    let mut out: Vec<f32> = Vec::new();
    while let Some(pkt) = reader.read_packet().map_err(|_| bad())? {
        let n = dec
            .decode_float(&pkt.data, &mut buf, false)
            .map_err(|_| bad())?;
        for frame in buf[..n * channels].chunks_exact(channels) {
            out.push(frame.iter().sum::<f32>() / f32::from(channel_count));
        }
        if out.len() > voice_local::MAX_SAMPLES {
            return Err("the recording is longer than 5 minutes".into());
        }
    }
    out.drain(..pre_skip.min(out.len()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in (needs ~100 MB of models): `PRAXI_VOICE_E2E_DIR` holds `models/voice/{whisper-tiny,
    /// silero-vad}` as the installer lays them out, plus a 16 kHz English `0.wav`.
    #[test]
    fn reads_a_real_recording_when_models_are_present() {
        let Some(dir) = std::env::var_os("PRAXI_VOICE_E2E_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let wav = std::fs::read(dir.join("0.wav")).unwrap();
        let samples = voice_local::decode_audio(&wav, "audio/wav").unwrap();
        let text = transcribe(&dir, "whisper-tiny", Some("en"), &samples).unwrap();
        assert!(text.to_lowercase().contains("nightfall"), "{text}");
        // Silence yields no text rather than an invented sentence.
        let quiet = vec![0.0f32; 16_000 * 2];
        let empty = transcribe(&dir, "whisper-tiny", Some("en"), &quiet).unwrap();
        assert!(empty.is_empty(), "{empty}");
    }

    #[test]
    fn garbage_is_not_ogg_opus() {
        assert!(decode_ogg_opus(b"definitely not ogg").is_err());
        assert!(decode_ogg_opus(&[]).is_err());
    }
}
