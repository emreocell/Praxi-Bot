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

/// Hosts the pinned downloads of this catalog come from (the llama.cpp release, the GGUF models
/// and the managed browser). A redirect may only lead to one of these (or a subdomain of one),
/// so a compromised or hijacked mirror cannot send the download somewhere else.
pub const PINNED_HOST_ROOTS: &[&str] = &[
    "huggingface.co",
    "hf.co",
    "github.com",
    "githubusercontent.com",
    "storage.googleapis.com",
];

/// `https`, no user name or password, the default port, and a host that is one of `roots` or a
/// subdomain of one. The rule for the first URL and for every redirect hop alike.
#[must_use]
pub fn is_pinned_https_url(url: &reqwest::Url, roots: &[&str]) -> bool {
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
    roots
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
}

/// Follow at most 5 redirects, each only to a URL that passes [`is_pinned_https_url`] for
/// `roots`. Anything else is not followed and surfaces as "server answered 3xx".
#[must_use]
pub fn pinned_redirects(roots: &'static [&'static str]) -> Policy {
    Policy::custom(move |attempt| {
        if attempt.previous().len() < 5 && is_pinned_https_url(attempt.url(), roots) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// http is only accepted towards this machine (tests and local mirrors).
fn url_allowed(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        url.username().is_empty()
            && url.password().is_none()
            && (url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
    })
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Bytes still needed for a file that has not been installed yet.
#[must_use]
pub fn missing_bytes(dest: &Path, size: u64) -> u64 {
    let partial = std::fs::symlink_metadata(part_path(dest))
        .ok()
        .filter(|m| m.is_file() && m.len() <= size)
        .map_or(0, |m| m.len());
    size - partial
}

/// Check the space needed for all pending downloads on this volume, including a reserve.
///
/// # Errors
/// Disk space could not be checked or is insufficient.
pub fn check_disk_space(dir: &Path, bytes: u64) -> Result<(), DownloadError> {
    let need_bytes = bytes.saturating_add(DISK_MARGIN);
    let free_bytes = fs4::available_space(dir).map_err(|e| io_err(&e))?;
    if free_bytes < need_bytes {
        return Err(DownloadError::NotEnoughDisk {
            need_bytes,
            free_bytes,
        });
    }
    Ok(())
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
        let ok_size =
            std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == size);
        Ok(ok_size && sha256_file(&path)?.eq_ignore_ascii_case(&want))
    })
    .await
    .map_err(|e| DownloadError::Io(e.to_string()))?
    .map_err(|e: std::io::Error| io_err(&e))
}

fn expected_range(response: &reqwest::Response, have: u64, size: u64) -> bool {
    let expected = format!("bytes {have}-{}/{size}", size - 1);
    response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        == Some(expected.as_str())
}

/// [`download_verified`] with the default HTTP client (the system proxy settings apply) and
/// redirects limited to [`PINNED_HOST_ROOTS`].
///
/// # Errors
/// See [`DownloadError`].
pub async fn download(
    spec: &DownloadSpec<'_>,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    // The first URL obeys the rule its redirects do (plain http is left to `download_verified`,
    // which only accepts this machine).
    let pinned = reqwest::Url::parse(spec.url)
        .is_ok_and(|url| url.scheme() == "http" || is_pinned_https_url(&url, PINNED_HOST_ROOTS));
    if !pinned {
        return Err(DownloadError::InsecureUrl);
    }
    let builder = reqwest::Client::builder().redirect(pinned_redirects(PINNED_HOST_ROOTS));
    download_verified(builder, spec, progress, cancel).await
}

/// Downloads `spec` to `spec.dest`, resuming a previous `.part`. `progress` gets the number of
/// bytes on disk so far. Setting `cancel` stops it between chunks and keeps the `.part`.
/// The caller supplies the redirect policy for its pinned source.
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
    let mut have = match tokio::fs::symlink_metadata(&part).await {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return Err(DownloadError::Io("invalid partial file".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(io_err(&e)),
    };
    if have > spec.size_bytes {
        let _ = tokio::fs::remove_file(&part).await;
        have = 0;
    }

    check_disk_space(parent, spec.size_bytes - have)?;

    if have < spec.size_bytes {
        let client = client_builder
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
            StatusCode::PARTIAL_CONTENT if have > 0 => {
                if !expected_range(&resp, have, spec.size_bytes) {
                    return Err(DownloadError::Network(
                        "unexpected range in download response".into(),
                    ));
                }
                tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(&part)
                    .await
                    .map_err(|e| io_err(&e))?
            }
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
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-range", "bytes 10-35/36")
                    .set_body_bytes(rest),
            )
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
    async fn rejects_a_wrong_range_without_touching_the_partial_file() {
        let server = MockServer::start().await;
        Mock::given(header_exists("range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-range", "bytes 9-35/36")
                    .set_body_bytes(&BODY[9..]),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        let part = part_path(&dest);
        std::fs::write(&part, &BODY[..10]).unwrap();
        let url = format!("{}/m.gguf", server.uri());
        let err = download_verified(
            builder(),
            &spec(&url, &dest, &good_sha()),
            &|_| {},
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DownloadError::Network(_)));
        assert_eq!(std::fs::read(&part).unwrap(), &BODY[..10]);
        assert!(!dest.exists());
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
        assert!(!url_allowed("http://localhost.example.com/m.gguf"));
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

    #[test]
    fn the_pinned_rule_checks_scheme_credentials_port_and_host() {
        let ok = |u: &str| is_pinned_https_url(&reqwest::Url::parse(u).unwrap(), PINNED_HOST_ROOTS);
        assert!(ok("https://huggingface.co/a/b"));
        assert!(ok("https://cas-bridge.xethub.hf.co/x"));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(ok(
            "https://storage.googleapis.com/chrome-for-testing-public/x.zip"
        ));
        assert!(
            ok("https://HuggingFace.co:443/a"),
            "the default port is the same thing"
        );
        assert!(!ok("http://huggingface.co/a"));
        assert!(!ok("https://huggingface.co:8443/a"));
        assert!(!ok("https://user@huggingface.co/a"));
        assert!(!ok("https://user:pw@huggingface.co/a"));
        assert!(!ok("https://huggingface.co.evil.example/a"));
        assert!(!ok("https://evilhuggingface.co/a"));
        assert!(!ok("https://huggingface.co@evil.example/a"));
        assert!(!ok("https://evil.googleapis.com/a"));
        assert!(!ok("https://127.0.0.1/a"));
        assert!(!ok("https://[::1]/a"));
    }

    /// A server whose first answer is a redirect to `location`.
    async fn redirecting_to(location: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_shared_downloader_does_not_follow_a_redirect_off_the_allow_list() {
        // The second server would answer with the right bytes: following the redirect would
        // install them. The policy must refuse before any request reaches it.
        let target = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(&target)
            .await;
        let hops = [
            format!("{}/m.gguf", target.uri()), // plain http, loopback, explicit port
            "https://user:pw@huggingface.co/m.gguf".to_owned(),
            "https://huggingface.co:8443/m.gguf".to_owned(),
            "https://huggingface.co.evil.example/m.gguf".to_owned(),
        ];
        for hop in hops {
            let first = redirecting_to(&hop).await;
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("m.gguf");
            let url = format!("{}/m.gguf", first.uri());
            let sha = good_sha();
            let err = download_verified(
                builder().redirect(pinned_redirects(PINNED_HOST_ROOTS)),
                &spec(&url, &dest, &sha),
                &|_| {},
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, DownloadError::Network(_)), "{hop}: {err:?}");
            assert!(!dest.exists(), "{hop}");
        }
        assert!(
            target.received_requests().await.unwrap().is_empty(),
            "a refused redirect must not be requested"
        );
    }
}
