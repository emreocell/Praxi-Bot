//! One-shot native voice worker. The daemon passes a bounded header and optional PCM/WAV/Opus
//! bytes over stdin, and reads a status byte followed by UTF-8 text or WAV over stdout.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use daemon_api::voice::VoiceControlError;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{voice_local, voice_local_engine};

pub const COMMAND: &str = "__offline-voice-worker";
const MAX_HEADER: u64 = 32 * 1024;
const MAX_AUDIO: u64 = 8 * 1024 * 1024;
const MAX_WAV: u64 = 16 * 1024 * 1024;
const MAX_TRANSCRIPT: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Transcribe {
        model: String,
        mime: String,
        language: Option<String>,
    },
    Synthesize {
        text: String,
        language: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Request {
    data_dir: PathBuf,
    operation: Operation,
}

pub async fn run(
    data_dir: &Path,
    operation: Operation,
    audio: &[u8],
) -> Result<Vec<u8>, VoiceControlError> {
    let (limit, timeout) = match &operation {
        Operation::Transcribe { .. } => (MAX_TRANSCRIPT, Duration::from_secs(120)),
        Operation::Synthesize { .. } => (MAX_WAV, Duration::from_secs(55)),
    };
    let failure =
        || VoiceControlError::Upstream("the offline voice worker stopped unexpectedly".into());
    let executable = std::env::current_exe().map_err(|_| failure())?;
    let request = Request {
        data_dir: data_dir.to_owned(),
        operation,
    };
    let mut header = serde_json::to_vec(&request).map_err(|_| failure())?;
    header.push(b'\n');
    if header.len() as u64 > MAX_HEADER || audio.len() as u64 > MAX_AUDIO {
        return Err(VoiceControlError::Invalid(
            "the offline voice request is too large".into(),
        ));
    }

    let mut cmd = tokio::process::Command::new(executable);
    cmd.arg(COMMAND)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear();
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|_| failure())?;
    let mut stdin = child.stdin.take().ok_or_else(failure)?;
    let mut stdout = child.stdout.take().ok_or_else(failure)?.take(limit + 2);

    let result = tokio::time::timeout(timeout, async {
        let mut output = Vec::new();
        tokio::try_join!(
            async move {
                stdin.write_all(&header).await?;
                stdin.write_all(audio).await?;
                stdin.shutdown().await
            },
            stdout.read_to_end(&mut output),
        )?;
        if output.len() as u64 > limit + 1 || !child.wait().await?.success() {
            return Err(io::Error::other("invalid worker output"));
        }
        Ok(output)
    })
    .await
    .map_err(|_| VoiceControlError::Upstream("the offline voice worker timed out".into()))?;
    let bytes = result.map_err(|_| failure())?;
    match bytes.split_first() {
        Some((0, payload)) => Ok(payload.to_vec()),
        Some((1, message)) => Err(VoiceControlError::Invalid(
            String::from_utf8_lossy(message).into_owned(),
        )),
        Some((2, _)) => Err(VoiceControlError::NotConfigured),
        _ => Err(failure()),
    }
}

pub fn worker() -> io::Result<()> {
    let (request, audio) = read_request(io::stdin().lock())?;
    let result = infer(request, &audio);
    let (status, payload) = match result {
        Ok(bytes) => (0, bytes),
        Err(VoiceControlError::Invalid(reason)) => (1, reason.into_bytes()),
        Err(VoiceControlError::NotConfigured) => (2, Vec::new()),
        Err(_) => (3, Vec::new()),
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(&[status])?;
    stdout.write_all(&payload)?;
    stdout.flush()
}

fn read_request(mut stdin: impl BufRead) -> io::Result<(Request, Vec<u8>)> {
    let mut header = Vec::new();
    stdin
        .by_ref()
        .take(MAX_HEADER)
        .read_until(b'\n', &mut header)?;
    if header.last() != Some(&b'\n') {
        return Err(io::Error::other("invalid voice worker header"));
    }
    let request = serde_json::from_slice(&header)?;
    let mut audio = Vec::new();
    stdin.take(MAX_AUDIO + 1).read_to_end(&mut audio)?;
    if audio.len() as u64 > MAX_AUDIO {
        return Err(io::Error::other("voice worker audio limit exceeded"));
    }
    Ok((request, audio))
}

fn infer(request: Request, audio: &[u8]) -> Result<Vec<u8>, VoiceControlError> {
    match request.operation {
        Operation::Transcribe {
            model,
            mime,
            language,
        } => {
            if !voice_local::stt_ready(&request.data_dir, &model) {
                return Err(VoiceControlError::Invalid(
                    "download the speech model in Settings > Dictation".into(),
                ));
            }
            let samples =
                voice_local::decode_audio(audio, &mime).map_err(VoiceControlError::Invalid)?;
            voice_local_engine::transcribe(&request.data_dir, &model, language.as_deref(), &samples)
                .map(String::into_bytes)
                .map_err(VoiceControlError::Upstream)
        }
        Operation::Synthesize { text, language } => {
            if !voice_local::is_installed(&request.data_dir, &voice_local::TTS) {
                return Err(VoiceControlError::NotConfigured);
            }
            voice_local_engine::synthesize(&request.data_dir, &text, &language)
                .map_err(VoiceControlError::Upstream)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_keeps_raw_pcm_and_language_intact() {
        let mut bytes = b"{\"data_dir\":\"data\",\"operation\":{\"Transcribe\":{\"model\":\"whisper-tiny\",\"mime\":\"audio/pcm\",\"language\":\"tr\"}}}\n".to_vec();
        let pcm = [0, 0x80, b'\n', 0xff];
        bytes.extend(pcm);
        let (request, audio) = read_request(bytes.as_slice()).unwrap();
        assert!(
            matches!(request.operation, Operation::Transcribe { language: Some(lang), .. } if lang == "tr")
        );
        assert_eq!(audio, pcm);
        assert!(read_request(b"{}".as_slice()).is_err());
    }
}
