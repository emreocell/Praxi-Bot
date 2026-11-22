//! Resumable, checksum-verified download of one pinned file.
//!
//! The bytes go to `<dest>.part`; only a file whose size and SHA-256 match the pin is renamed to
//! `dest`. A stopped download keeps its `.part` and continues with an HTTP `Range` request next
//! time; a file that fails the checksum is deleted. There is a free-disk check up front.

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use reqwest::{StatusCode, header, redirect::Policy};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Free space kept after the download, on top of the file itself.
const DISK_MARGIN: u64 = 512 * 1024 * 1024;
/// A stalled connection is given up after this long without a byte.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What to fetch and what it must look like.
#[derive(Debug, Clone, Copy)]
pub struct DownloadSpec<'a> {
    pub url: &'a str,
    pub dest: &'a Path,
    /// Lower- or upper-case hex SHA-256 of the complete file.
    pub sha256: &'a str,
    pub size_bytes: u64,
}

/// Why a download did not finish. `Display` is plain language for the owner.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DownloadError {
    #[error("not enough free disk space: {need_bytes} bytes needed, {free_bytes} free")]
    NotEnoughDisk { need_bytes: u64, free_bytes: u64 },
    #[error("download stopped")]
    Cancelled,
    #[error("the file arrived damaged or different from the expected one and was deleted")]
    ChecksumMismatch,
    #[error("the download ended early; run it again to continue")]
    Incomplete,
    #[error("the server sent more data than expected")]
    TooLarge,
    #[error("only secure (https) downloads are allowed")]
    InsecureUrl,
    #[error("network problem: {0}")]
    Network(String),
    #[error("could not write the file: {0}")]
    Io(String),
}

fn io_err(e: &std::io::Error) -> DownloadError {
    DownloadError::Io(e.kind().to_string())
}

/// http is only accepted towards this machine (tests and local mirrors).
fn url_allowed(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost")
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a file, streamed. Blocking.
///
/// # Errors
/// I/O errors while reading.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

async fn verified(path: PathBuf, want: String, size: u64) -> Result<bool, DownloadError> {
    tokio::task::spawn_blocking(move || {
        let ok_size = std::fs::metadata(&path).is_ok_and(|m| m.len() == size);
        Ok(ok_size && sha256_file(&path)?.eq_ignore_ascii_case(&want))
    })
    .await
    .map_err(|e| DownloadError::Io(e.to_string()))?
    .map_err(|e: std::io::Error| io_err(&e))
}

/// [`download_verified`] with the default HTTP client (the system proxy settings apply).
///
/// # Errors
/// See [`DownloadError`].
pub async fn download(
    spec: &DownloadSpec<'_>,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    download_verified(reqwest::Client::builder(), spec, progress, cancel).await
}

/// Downloads `spec` to `spec.dest`, resuming a previous `.part`. `progress` gets the number of
/// bytes on disk so far. Setting `cancel` stops it between chunks and keeps the `.part`.
///
/// # Errors
/// See [`DownloadError`].
pub async fn download_verified(
    client_builder: reqwest::ClientBuilder,
    spec: &DownloadSpec<'_>,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    if !url_allowed(spec.url) {
        return Err(DownloadError::InsecureUrl);
    }
    if verified(
        spec.dest.to_path_buf(),
        spec.sha256.to_owned(),
        spec.size_bytes,
    )
    .await?
    {
        progress(spec.size_bytes);
        return Ok(());
    }
    let parent = spec.dest.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| io_err(&e))?;
    let part = part_path(spec.dest);
    let mut have = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
    if have > spec.size_bytes {
        let _ = tokio::fs::remove_file(&part).await;
        have = 0;
    }

    let need = spec.size_bytes - have + DISK_MARGIN;
    if let Ok(free) = fs4::available_space(parent)
        && free < need
    {
        return Err(DownloadError::NotEnoughDisk {
            need_bytes: need,
            free_bytes: free,
        });
    }

    if have < spec.size_bytes {
        let client = client_builder
            .redirect(Policy::custom(|attempt| {
                if attempt.url().scheme() == "https" && attempt.previous().len() < 5 {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .read_timeout(IDLE_TIMEOUT)
            .build()
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        let mut req = client.get(spec.url);
        if have > 0 {
            req = req.header(header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| DownloadError::Network(e.without_url().to_string()))?;
        let mut file = match resp.status() {
            StatusCode::PARTIAL_CONTENT if have > 0 => tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .await
                .map_err(|e| io_err(&e))?,
            StatusCode::OK => {
                // The server ignored the range: start over.
                have = 0;
                tokio::fs::File::create(&part)
                    .await
                    .map_err(|e| io_err(&e))?
            }
            s => return Err(DownloadError::Network(format!("server answered {s}"))),
        };
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| DownloadError::Network(e.without_url().to_string()))?
        {
            if cancel.load(Ordering::Relaxed) {
                let _ = file.flush().await;
                return Err(DownloadError::Cancelled);
            }
            have += chunk.len() as u64;
            if have > spec.size_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(DownloadError::TooLarge);
            }
            file.write_all(&chunk).await.map_err(|e| io_err(&e))?;
            progress(have);
        }
        file.flush().await.map_err(|e| io_err(&e))?;
    }

    if have != spec.size_bytes {
        return Err(DownloadError::Incomplete);
    }
    if !verified(part.clone(), spec.sha256.to_owned(), spec.size_bytes).await? {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(DownloadError::ChecksumMismatch);
    }
    tokio::fs::rename(&part, spec.dest)
        .await
        .map_err(|e| io_err(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::header_exists};

    use super::*;

    const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    fn spec<'a>(url: &'a str, dest: &'a Path, sha: &'a str) -> DownloadSpec<'a> {
        DownloadSpec {
            url,
            dest,
            sha256: sha,
            size_bytes: BODY.len() as u64,
        }
    }

    fn good_sha() -> String {
        hex(&Sha256::digest(BODY))
    }

    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder().no_proxy()
    }

    #[tokio::test]
    async fn downloads_verifies_and_renames() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let url = format!("{}/m.gguf", server.uri());
        let seen = AtomicU64::new(0);
        let sha = good_sha();
        download_verified(
            builder(),
            &spec(&url, &dest, &sha),
            &|n| seen.store(n, Ordering::Relaxed),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
        assert!(!part_path(&dest).exists());
        assert_eq!(seen.load(Ordering::Relaxed), BODY.len() as u64);
    }

    #[tokio::test]
    async fn a_wrong_checksum_deletes_the_file_and_is_refused() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let url = format!("{}/m.gguf", server.uri());
        let wrong = "0".repeat(64);
        let err = download_verified(
            builder(),
            &spec(&url, &dest, &wrong),
            &|_| {},
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert_eq!(err, DownloadError::ChecksumMismatch);
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    #[tokio::test]
    async fn resumes_from_a_part_file_with_a_range_request() {
        let server = MockServer::start().await;
        let rest = &BODY[10..];
        Mock::given(header_exists("range"))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(rest))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(part_path(&dest), &BODY[..10]).unwrap();
        let url = format!("{}/m.gguf", server.uri());
        let sha = good_sha();
        download_verified(
            builder(),
            &spec(&url, &dest, &sha),
            &|_| {},
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    }

    #[tokio::test]
    async fn refuses_plain_http_to_other_hosts_and_a_full_disk() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let sha = good_sha();
        let err = download_verified(
            builder(),
            &spec("http://example.com/m.gguf", &dest, &sha),
            &|_| {},
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert_eq!(err, DownloadError::InsecureUrl);
        // A pin larger than any disk cannot fit: refused before any request is made.
        let huge = DownloadSpec {
            url: "http://127.0.0.1:9/x",
            dest: &dest,
            sha256: &sha,
            size_bytes: u64::MAX / 2,
        };
        let err = download_verified(builder(), &huge, &|_| {}, &AtomicBool::new(false))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DownloadError::NotEnoughDisk { .. }),
            "{err:?}"
        );
    }
}
