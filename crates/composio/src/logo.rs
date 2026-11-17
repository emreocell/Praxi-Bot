//! App logos: fetched by the daemon, validated, cached on disk and served through the daemon's
//! authenticated local route. The webview never loads a remote image (its CSP allows only local
//! ones), and a logo is untrusted third-party content:
//!
//! * only https URLs on Composio's logo hosts, no credentials, no port, no redirects;
//! * at most [`MAX_LOGO_BYTES`], PNG / WebP / SVG by their own bytes (the declared type is not
//!   trusted);
//! * an SVG that carries anything active (script, event handlers, foreign objects, external
//!   references, entities, embedded raster) is REFUSED, never "cleaned";
//! * served with `nosniff` and a CSP that forbids everything but inline styles.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Largest logo accepted (and cached).
pub const MAX_LOGO_BYTES: usize = 96 * 1024;

/// A validated logo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logo {
    pub bytes: Vec<u8>,
    /// `image/png`, `image/webp` or `image/svg+xml`.
    pub content_type: &'static str,
}

/// Why a logo was not served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoError {
    /// Not an https URL on an allowed host.
    BadUrl,
    /// The download failed.
    Fetch,
    /// Larger than [`MAX_LOGO_BYTES`].
    TooLarge,
    /// Not a PNG, WebP or SVG.
    NotImage,
    /// An SVG with active content.
    Unsafe,
}

/// Hosts a logo may come from (Composio's own CDN names).
fn allowed_host(host: &str) -> bool {
    host == "composio.dev" || host.ends_with(".composio.dev")
}

/// Whether `url` may be fetched for a logo.
#[must_use]
pub fn allowed_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && !url.chars().any(|c| c.is_control() || c == '\\')
        && u.host_str().is_some_and(allowed_host)
}

/// Substrings that make an SVG active or externally linked.
const FORBIDDEN: &[&str] = &[
    "<script",
    "<foreignobject",
    "<iframe",
    "<object",
    "<embed",
    "<!entity",
    "<!doctype",
    "<image",
    "<use",
    "<a ",
    "<a>",
    "<animate",
    "<set ",
    "<style",
    "javascript:",
    "data:",
    "http://",
    "https://",
    "xlink:href",
    "href=",
    "@import",
    "url(",
];

/// Check an SVG for anything active. Refuses rather than rewrites.
fn check_svg(bytes: &[u8]) -> Result<(), LogoError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(LogoError::NotImage);
    };
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<svg") {
        return Err(LogoError::NotImage);
    }
    if FORBIDDEN.iter().any(|f| lower.contains(f)) {
        // The SVG namespace declaration is the one legitimate URL.
        let stripped = lower.replace("http://www.w3.org/2000/svg", "");
        if FORBIDDEN.iter().any(|f| stripped.contains(f)) {
            return Err(LogoError::Unsafe);
        }
    }
    // Event handler attributes: ` onload=`, `\nonclick =` ...
    let bytes = lower.as_bytes();
    for (i, w) in bytes.windows(3).enumerate() {
        if w == b" on" || w == b"\non" || w == b"\ton" {
            let rest = &lower[i + 3..];
            let name: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
            let after = rest[name.len()..].trim_start();
            if !name.is_empty() && after.starts_with('=') {
                return Err(LogoError::Unsafe);
            }
        }
    }
    Ok(())
}

/// The content type of `bytes` by their own signature, after the safety checks.
///
/// # Errors
/// [`LogoError::TooLarge`], [`LogoError::NotImage`] or [`LogoError::Unsafe`].
pub fn classify(bytes: &[u8]) -> Result<&'static str, LogoError> {
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(LogoError::TooLarge);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok("image/webp");
    }
    let head = &bytes[..bytes.len().min(512)];
    let trimmed = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(head, |i| &head[i..]);
    if trimmed.starts_with(b"<svg") || trimmed.starts_with(b"<?xml") {
        check_svg(bytes)?;
        return Ok("image/svg+xml");
    }
    Err(LogoError::NotImage)
}

fn extension(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "svg",
    }
}

/// Fetch `url` (checked) with every limit applied.
///
/// # Errors
/// See [`LogoError`].
pub async fn fetch(url: &str) -> Result<Logo, LogoError> {
    if !allowed_url(url) {
        return Err(LogoError::BadUrl);
    }
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(5))
        .https_only(true)
        .user_agent(concat!("praxi-bot-composio/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| LogoError::Fetch)?;
    let mut response = http.get(url).send().await.map_err(|_| LogoError::Fetch)?;
    if !response.status().is_success() {
        return Err(LogoError::Fetch);
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_LOGO_BYTES as u64)
    {
        return Err(LogoError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| LogoError::Fetch)? {
        if bytes.len() + chunk.len() > MAX_LOGO_BYTES {
            return Err(LogoError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let content_type = classify(&bytes)?;
    Ok(Logo {
        bytes,
        content_type,
    })
}

fn cache_dir(data: &Path) -> PathBuf {
    data.join("composio-logos")
}

/// A logo already cached for `slug` (re-validated: the file may have been tampered with).
#[must_use]
pub fn cached(data: &Path, slug: &str) -> Option<Logo> {
    ["png", "webp", "svg"].iter().find_map(|ext| {
        let bytes = std::fs::read(cache_dir(data).join(format!("{slug}.{ext}"))).ok()?;
        let content_type = classify(&bytes).ok()?;
        (extension(content_type) == *ext).then_some(Logo {
            bytes,
            content_type,
        })
    })
}

/// Store a validated logo.
pub fn store(data: &Path, slug: &str, logo: &Logo) {
    let dir = cache_dir(data);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(
            dir.join(format!("{slug}.{}", extension(logo.content_type))),
            &logo.bytes,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn only_https_urls_on_composio_hosts_are_fetched() {
        assert!(allowed_url("https://logos.composio.dev/api/gmail"));
        for bad in [
            "http://logos.composio.dev/x.png",
            "https://evil.example/x.png",
            "https://composio.dev.evil.example/x.png",
            "https://user:pw@logos.composio.dev/x.png",
            "https://logos.composio.dev:8443/x.png",
            "https://127.0.0.1/x.png",
            "file:///etc/passwd",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
    }

    #[test]
    fn images_are_told_by_their_bytes_and_oversized_or_other_files_are_refused() {
        assert_eq!(classify(PNG), Ok("image/png"));
        assert_eq!(classify(b"RIFF\0\0\0\0WEBPVP8 "), Ok("image/webp"));
        assert_eq!(
            classify(br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h24v24H0z"/></svg>"#),
            Ok("image/svg+xml")
        );
        assert_eq!(classify(b"GIF89a....."), Err(LogoError::NotImage));
        assert_eq!(
            classify(b"<html><body>hi</body></html>"),
            Err(LogoError::NotImage)
        );
        assert_eq!(classify(b"MZ\x90\0"), Err(LogoError::NotImage));
        let mut big = PNG.to_vec();
        big.resize(MAX_LOGO_BYTES + 1, 0);
        assert_eq!(classify(&big), Err(LogoError::TooLarge));
    }

    #[test]
    fn an_svg_with_anything_active_is_refused_not_cleaned() {
        for bad in [
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><path d="M0 0"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0" onclick = "x()"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><div/></foreignObject></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><image href="https://evil.example/x.png"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><a xlink:href="javascript:alert(1)"><path d="M0 0"/></a></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(https://evil.example/a.css);</style></svg>"#,
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
            r##"<svg xmlns="http://www.w3.org/2000/svg"><use href="#a"/></svg>"##,
        ] {
            assert_eq!(classify(bad.as_bytes()), Err(LogoError::Unsafe), "{bad}");
        }
    }

    #[test]
    fn the_cache_round_trips_and_a_tampered_file_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let logo = Logo {
            bytes: PNG.to_vec(),
            content_type: "image/png",
        };
        store(dir.path(), "gmail", &logo);
        assert_eq!(cached(dir.path(), "gmail"), Some(logo));
        std::fs::write(
            dir.path().join("composio-logos/gmail.png"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>",
        )
        .unwrap();
        assert_eq!(cached(dir.path(), "gmail"), None);
    }
}
