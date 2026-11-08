//! Where skills come from: local folders, git repositories, index documents.
//!
//! Network and git access sit behind the [`Fetcher`] and [`GitBackend`]
//! traits so the hub can be tested without either. The defaults shell out to
//! the `curl` and `git` command-line tools (no HTTP client dependency).

use std::io::Read;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use serde::Deserialize;

use super::HubError;

/// Largest index document accepted.
pub const MAX_INDEX_BYTES: usize = 1024 * 1024;

/// Redirects followed when downloading an index. Kept small so a hostile
/// server cannot bounce the client around indefinitely.
const MAX_REDIRS: &str = "5";

/// What to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// A folder containing `SKILL.md`.
    Local(std::path::PathBuf),
    /// A git repository (shallow-cloned).
    Git {
        /// Clone URL. Only `https://` and `ssh://` are accepted.
        url: String,
        /// Branch, tag or full commit hash to pin; `None` = default branch.
        rev: Option<String>,
        /// Skill folder inside the repository; `None` = repository root.
        subdir: Option<String>,
    },
    /// An entry of an index JSON served over `https://`.
    Index {
        /// URL of the index document.
        url: String,
        /// Skill name inside the index.
        name: String,
    },
}

/// Downloads small documents (index JSON).
pub trait Fetcher: Send + Sync {
    /// GET `url` and return the body.
    ///
    /// # Errors
    /// Any transport failure or body over [`MAX_INDEX_BYTES`].
    fn get(&self, url: &str) -> Result<Vec<u8>, HubError>;
}

/// Fetches with the `curl` CLI (`https` only, size and time limited).
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlFetcher;

fn curl_command_args(url: &str) -> Vec<String> {
    // `--max-filesize` is intentionally absent: curl ignores it for chunked
    // responses and for responses without `Content-Length`, and `output()`
    // would buffer the body before a length check could run.
    vec![
        "--fail".to_string(),
        "--silent".to_string(),
        "--show-error".to_string(),
        "--location".to_string(),
        "--proto".to_string(),
        "=https".to_string(),
        "--proto-redir".to_string(),
        "=https".to_string(),
        "--max-time".to_string(),
        "30".to_string(),
        "--max-redirs".to_string(),
        MAX_REDIRS.to_string(),
        "--output".to_string(),
        "-".to_string(),
        "--".to_string(),
        url.to_string(),
    ]
}

/// Read `reader` until EOF. Stop and return an error once more than `max`
/// bytes have been seen, without pulling the rest of the stream.
///
/// # Errors
/// [`HubError::Source`] when the stream exceeds `max` or a read fails.
pub(super) fn read_capped<R: Read>(mut reader: R, max: usize) -> Result<Vec<u8>, HubError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| HubError::Source(format!("read index: {e}")))?;
        if n == 0 {
            break;
        }
        if buf.len().saturating_add(n) > max {
            return Err(HubError::Source("index document is too large".into()));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

fn drain_capped(mut reader: impl Read, max: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(buf.len());
                if room == 0 {
                    continue;
                }
                buf.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    buf
}

impl Fetcher for CurlFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, HubError> {
        let mut child = Command::new("curl")
            .args(curl_command_args(url))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| HubError::Source(format!("cannot run curl: {e}")))?;
        let stderr = child.stderr.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HubError::Source("curl stdout missing".into()))?;
        let err_thread = std::thread::spawn(move || {
            stderr.map_or_else(Vec::new, |pipe| drain_capped(pipe, 64 * 1024))
        });
        let body = read_capped(stdout, MAX_INDEX_BYTES);
        if body.is_err() {
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|e| HubError::Source(format!("curl wait: {e}")))?;
        let stderr_buf = err_thread.join().unwrap_or_default();
        let body = body?;
        if !status.success() {
            return Err(HubError::Source(format!(
                "curl failed: {}",
                String::from_utf8_lossy(&stderr_buf).trim()
            )));
        }
        Ok(body)
    }
}

/// Checks a git revision out into a directory.
pub trait GitBackend: Send + Sync {
    /// Shallow-fetch `rev` (or the default branch) of `url` into the empty
    /// directory `dest` and return the full commit hash that was checked out.
    ///
    /// # Errors
    /// Any git failure.
    fn checkout(&self, url: &str, rev: Option<&str>, dest: &Path) -> Result<String, HubError>;
}

/// Uses the `git` CLI: `init` + `fetch --depth 1` + `checkout`, which works
/// for branches, tags and full commit hashes alike.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitCli;

struct GitOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn nonexistent_hooks(dest: &Path) -> Result<std::path::PathBuf, HubError> {
    let parent = dest.parent().ok_or_else(|| {
        HubError::Git("git directory has no parent for the hooks override".into())
    })?;
    let path = std::path::absolute(parent.join("praxi-no-hooks"))
        .map_err(|e| HubError::Git(format!("hooks path: {e}")))?;
    if path.as_os_str().to_string_lossy().contains(['=', '\n']) {
        return Err(HubError::Git(
            "refusing to run git: unsafe hooks path".into(),
        ));
    }
    if path.exists() {
        return Err(HubError::Git(
            "refusing to run git: hooks directory already exists".into(),
        ));
    }
    Ok(path)
}

fn push_config(cmd: &mut Command, key_value: &str) {
    cmd.arg("-c").arg(key_value);
}

fn spawn_git(dir: &Path, args: &[String], extra_c: &[String]) -> Result<GitOutput, HubError> {
    let hooks = nonexistent_hooks(dir)?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for key_value in [
        "protocol.file.allow=user",
        "protocol.ext.allow=never",
        "core.fsmonitor=",
        "uploadpack.packObjectsHook=",
        "submodule.recurse=false",
        "fetch.recurseSubmodules=false",
        "core.symlinks=false",
        "advice.detachedHead=false",
    ] {
        push_config(&mut cmd, key_value);
    }
    push_config(&mut cmd, &format!("core.hooksPath={}", hooks.display()));
    cmd.args(extra_c).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "30")
        .env("GIT_CONFIG_COUNT", "0");
    let out = cmd
        .output()
        .map_err(|e| HubError::Git(format!("cannot run git: {e}")))?;
    Ok(GitOutput {
        status: out.status,
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

fn git_text_raw(dir: &Path, args: &[&str]) -> Result<String, HubError> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = spawn_git(dir, &owned, &[])?;
    if !out.status.success() {
        return Err(HubError::Git(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn valid_filter_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn parse_filter_names(stdout: &str) -> Result<Vec<String>, HubError> {
    let mut names = Vec::new();
    for entry in stdout.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let key = entry.split('\n').next().unwrap_or("");
        let Some(rest) = key.strip_prefix("filter.") else {
            continue;
        };
        let Some((name, _var)) = rest.rsplit_once('.') else {
            continue;
        };
        if !valid_filter_name(name) {
            return Err(HubError::Git(format!(
                "refusing to run git: unsupported filter driver name {name:?}"
            )));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

fn read_scoped_filters(dir: &Path, scope: &str) -> Result<Vec<String>, HubError> {
    let args = vec![
        "config".to_string(),
        scope.to_string(),
        "--null".to_string(),
        "--get-regexp".to_string(),
        r"^filter\.".to_string(),
    ];
    let out = spawn_git(dir, &args, &[])?;
    match out.status.code() {
        Some(0) => parse_filter_names(&String::from_utf8_lossy(&out.stdout)),
        Some(1) => Ok(Vec::new()),
        Some(128) => {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("not a git repository") || err.contains("can only be used inside") {
                Ok(Vec::new())
            } else {
                Err(HubError::Git(format!(
                    "git config {scope} failed: {}",
                    err.trim()
                )))
            }
        }
        _ => Err(HubError::Git(format!(
            "git config {scope} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

fn configured_filter_names(dir: &Path) -> Result<Vec<String>, HubError> {
    let mut names = Vec::new();
    for scope in ["--system", "--global", "--local"] {
        for name in read_scoped_filters(dir, scope)? {
            if !names.iter().any(|existing| existing == &name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

fn safe_git_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['-', '/'])
        && !path.contains(['\\', ':'])
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn collect_attr_filters(text: &str, names: &mut Vec<String>) -> Result<(), HubError> {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or(line);
        for token in line.split_whitespace() {
            let Some(name) = token.strip_prefix("filter=") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            if !valid_filter_name(name) {
                return Err(HubError::Git(format!(
                    "refusing to run git: unsupported filter driver name {name:?}"
                )));
            }
            if !names.iter().any(|existing| existing == name) {
                names.push(name.to_string());
            }
        }
    }
    Ok(())
}

fn attribute_filter_names(dir: &Path) -> Result<Vec<String>, HubError> {
    if !dir.join(".git").join("FETCH_HEAD").exists() {
        return Ok(Vec::new());
    }
    let listed = git_text_raw(dir, &["ls-tree", "-r", "-z", "--name-only", "FETCH_HEAD"])?;
    let mut names = Vec::new();
    let mut seen_files = 0usize;
    for path in listed.split('\0') {
        if path.is_empty() || !(path == ".gitattributes" || path.ends_with("/.gitattributes")) {
            continue;
        }
        if !safe_git_path(path) {
            return Err(HubError::Git(format!(
                "refusing to read attributes path `{path}`"
            )));
        }
        seen_files += 1;
        if seen_files > 32 {
            return Err(HubError::Git("too many .gitattributes files".into()));
        }
        let spec = format!("FETCH_HEAD:{path}");
        let size_text = git_text_raw(dir, &["cat-file", "-s", &spec])?;
        let size: u64 = size_text
            .trim()
            .parse()
            .map_err(|_| HubError::Git(format!("bad attributes size for `{path}`")))?;
        if size > u64::try_from(MAX_INDEX_BYTES).unwrap_or(u64::MAX) {
            return Err(HubError::Git(format!(
                "attributes file `{path}` is too large"
            )));
        }
        let text = git_text_raw(dir, &["cat-file", "blob", &spec])?;
        collect_attr_filters(&text, &mut names)?;
    }
    Ok(names)
}

fn blank_filter_args(names: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(names.len() * 8);
    for name in names {
        for var in ["clean", "smudge", "process"] {
            args.push("-c".to_string());
            args.push(format!("filter.{name}.{var}="));
        }
        args.push("-c".to_string());
        args.push(format!("filter.{name}.required=false"));
    }
    args
}

fn all_filter_blanks(dir: &Path) -> Result<Vec<String>, HubError> {
    let mut names = configured_filter_names(dir)?;
    for name in attribute_filter_names(dir)? {
        if !names.iter().any(|existing| existing == &name) {
            names.push(name);
        }
    }
    Ok(blank_filter_args(&names))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, HubError> {
    let extra = all_filter_blanks(dir)?;
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = spawn_git(dir, &owned, &extra)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(HubError::Git(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

impl GitBackend for GitCli {
    fn checkout(&self, url: &str, rev: Option<&str>, dest: &Path) -> Result<String, HubError> {
        validate_git_url(url)?;
        if let Some(r) = rev {
            validate_rev(r)?;
        }
        git(dest, &["init", "--quiet"])?;
        git(dest, &["remote", "add", "origin", url])?;
        git(
            dest,
            &[
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "--no-recurse-submodules",
                "origin",
                rev.unwrap_or("HEAD"),
            ],
        )?;
        git(dest, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
        git(dest, &["rev-parse", "HEAD"])
    }
}

fn has_allowed_git_scheme(url: &str) -> bool {
    let bytes = url.as_bytes();
    for scheme in [b"https://".as_slice(), b"ssh://".as_slice()] {
        if bytes.len() > scheme.len() && bytes[..scheme.len()].eq_ignore_ascii_case(scheme) {
            return true;
        }
    }
    false
}

/// Reject git URLs that are not `https://` or `ssh://`.
///
/// `file://`, `http://`, `git://`, UNC paths, local paths, and `ext::`
/// transports are refused. A leading `-` cannot be an option because the URL
/// is a separate argument, but it is still rejected.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_git_url(url: &str) -> Result<(), HubError> {
    let bad = url.is_empty()
        || url.starts_with('-')
        || url.contains("::")
        || url.contains('\\')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || !has_allowed_git_scheme(url);
    if bad {
        return Err(HubError::Source(format!("unsafe git url `{url}`")));
    }
    Ok(())
}

/// Require a full Git object id: 40 or 64 hexadecimal digits.
///
/// # Errors
/// [`HubError::Source`] when `commit` is missing, short, or not hexadecimal.
pub fn validate_commit_pin(commit: &str) -> Result<(), HubError> {
    let ok = matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(HubError::Source(format!(
            "index commit must be a full 40- or 64-character hexadecimal hash, not `{commit}`"
        )))
    }
}

/// Reject revisions that are not plain ref names or hashes.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_rev(rev: &str) -> Result<(), HubError> {
    let ok = !rev.is_empty()
        && !rev.starts_with('-')
        && !rev.contains("..")
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if ok {
        Ok(())
    } else {
        Err(HubError::Source(format!("invalid git revision `{rev}`")))
    }
}

/// A `subdir` must be a plain relative path without `..`.
///
/// # Errors
/// [`HubError::Source`] describing the problem.
pub fn validate_subdir(subdir: &str) -> Result<(), HubError> {
    let p = Path::new(subdir);
    let ok = !subdir.is_empty()
        && !p.is_absolute()
        && p.components().all(|c| matches!(c, Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(HubError::Source(format!("invalid subdirectory `{subdir}`")))
    }
}

/// An index document: `{"version":1,"skills":[{...}]}`.
#[derive(Debug, Deserialize)]
pub struct IndexFile {
    /// Schema version (currently 1).
    #[serde(default)]
    pub version: u32,
    /// Listed skills.
    pub skills: Vec<IndexEntry>,
}

/// One skill listed in an index.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexEntry {
    /// Skill name.
    pub name: String,
    /// Short description (display only).
    #[serde(default)]
    pub description: String,
    /// Git clone URL.
    pub git: String,
    /// Full 40- or 64-character commit hash. Required; a missing or short
    /// value is rejected.
    #[serde(default)]
    pub commit: Option<String>,
    /// Folder inside the repository.
    #[serde(default)]
    pub subdir: Option<String>,
    /// Expected content fingerprint (as printed by the scan report); verified
    /// after fetching when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Parse an index document.
///
/// # Errors
/// [`HubError::Source`] for malformed JSON or unsupported versions.
pub fn parse_index(bytes: &[u8]) -> Result<IndexFile, HubError> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(HubError::Source("index document is too large".into()));
    }
    let idx: IndexFile = serde_json::from_slice(bytes)
        .map_err(|e| HubError::Source(format!("invalid index JSON: {e}")))?;
    if idx.version > 1 {
        return Err(HubError::Source(format!(
            "unsupported index version {}",
            idx.version
        )));
    }
    for entry in &idx.skills {
        let Some(commit) = entry.commit.as_deref().filter(|c| !c.is_empty()) else {
            return Err(HubError::Source(format!(
                "index entry `{}` is missing a commit pin",
                entry.name
            )));
        };
        validate_commit_pin(commit)?;
        validate_git_url(&entry.git)?;
    }
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::path::Path;
    use std::process::Command;

    use super::{
        MAX_INDEX_BYTES, curl_command_args, git, read_capped, validate_commit_pin, validate_git_url,
    };

    #[test]
    fn rejects_local_file_http_git_and_unc_urls() {
        for url in [
            "file:///C:/repo",
            "file://server/share",
            "http://example.com/a.git",
            "git://example.com/a.git",
            r"\\server\share",
            "//server/share",
            r"C:\repo",
            "/tmp/repo",
            "../repo",
            "git@github.com:org/repo.git",
            "ext::sh",
            "-https://example.com/a.git",
            "https://example.com/a.git\n",
            "ssh://example.com/a.git\r",
            "https://",
            "ssh://",
            "",
        ] {
            assert!(validate_git_url(url).is_err(), "{url}");
        }
        assert!(validate_git_url("https://example.com/a.git").is_ok());
        assert!(validate_git_url("ssh://git@github.com/org/repo.git").is_ok());
        assert!(validate_git_url("HTTPS://example.com/a.git").is_ok());
    }

    #[test]
    fn commit_pin_requires_full_hex() {
        assert!(validate_commit_pin("abc").is_err());
        assert!(validate_commit_pin(&"a".repeat(39)).is_err());
        assert!(validate_commit_pin(&"g".repeat(40)).is_err());
        assert!(validate_commit_pin(&"ab".repeat(20)).is_ok());
        assert!(validate_commit_pin(&"cd".repeat(32)).is_ok());
        assert_eq!(MAX_INDEX_BYTES, 1024 * 1024);
    }

    struct ChunkReader {
        left: usize,
        chunk: usize,
        read: usize,
    }

    impl Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Ok(0);
            }
            let n = self.left.min(self.chunk).min(buf.len());
            buf[..n].fill(b'a');
            self.left -= n;
            self.read += n;
            Ok(n)
        }
    }

    #[test]
    fn read_capped_stops_before_the_rest_of_the_body() {
        let mut reader = ChunkReader {
            left: 10_000,
            chunk: 100,
            read: 0,
        };
        let err = read_capped(&mut reader, 250).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(
            reader.read < 10_000,
            "buffered the whole body ({})",
            reader.read
        );
        assert!(reader.read <= 250 + 100, "read {}", reader.read);
        let mut exact = ChunkReader {
            left: 250,
            chunk: 80,
            read: 0,
        };
        let body = read_capped(&mut exact, 250).unwrap();
        assert_eq!(body.len(), 250);
    }

    #[test]
    fn curl_index_fetch_limits_redirects_without_max_filesize() {
        let args = curl_command_args("https://example.com/index.json");
        assert!(args.windows(2).any(|pair| pair == ["--max-redirs", "5"]));
        assert!(args.windows(2).any(|pair| pair == ["--proto", "=https"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--proto-redir", "=https"])
        );
        assert!(!args.iter().any(|arg| arg == "--max-filesize"));
        let split = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[split + 1], "https://example.com/index.json");
    }

    fn raw_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fwd(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    #[test]
    fn hardened_checkout_skips_hooks_filters_and_submodules() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        let evil = tmp.path().join("evil");
        let dest = tmp.path().join("dest");
        let marker = tmp.path().join("marker.txt");
        let script = tmp.path().join("mark.sh");
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&evil).unwrap();
        fs::create_dir_all(&dest).unwrap();
        let marker_fwd = fwd(&marker);
        let script_fwd = fwd(&script);
        fs::write(
            &script,
            format!("#!/bin/sh\necho RAN >> '{marker_fwd}'\ncat\n"),
        )
        .unwrap();
        let cmd = format!("sh {script_fwd}");

        raw_git(&evil, &["init", "--quiet", "-b", "main"]);
        raw_git(&evil, &["config", "user.email", "t@example.com"]);
        raw_git(&evil, &["config", "user.name", "t"]);
        fs::write(evil.join("pwned.txt"), "pwned\n").unwrap();
        raw_git(&evil, &["add", "pwned.txt"]);
        raw_git(&evil, &["commit", "--quiet", "-m", "e"]);

        raw_git(&origin, &["init", "--quiet", "-b", "main"]);
        raw_git(&origin, &["config", "user.email", "t@example.com"]);
        raw_git(&origin, &["config", "user.name", "t"]);
        raw_git(&origin, &["config", "core.autocrlf", "false"]);
        fs::write(origin.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        fs::write(origin.join("a.txt"), "hello\n").unwrap();
        fs::write(
            origin.join(".gitmodules"),
            format!(
                "[submodule \"evil\"]\n\tpath = evil\n\turl = {}\n",
                fwd(&evil)
            ),
        )
        .unwrap();
        raw_git(&origin, &["add", "."]);
        raw_git(&origin, &["commit", "--quiet", "-m", "c"]);

        git(&dest, &["init", "--quiet"]).unwrap();
        for (key, value) in [
            ("filter.evil.smudge", cmd.as_str()),
            ("filter.evil.clean", cmd.as_str()),
            ("filter.evil.process", cmd.as_str()),
            ("filter.evil.required", "true"),
            ("core.fsmonitor", cmd.as_str()),
            ("uploadpack.packObjectsHook", cmd.as_str()),
            ("submodule.recurse", "true"),
            ("fetch.recurseSubmodules", "true"),
            ("core.autocrlf", "false"),
        ] {
            raw_git(&dest, &["config", key, value]);
        }
        let hook = dest.join(".git").join("hooks").join("post-checkout");
        fs::write(&hook, format!("#!/bin/sh\necho HOOK >> '{marker_fwd}'\n")).unwrap();
        git(&dest, &["remote", "add", "origin", &fwd(&origin)]).unwrap();
        git(
            &dest,
            &[
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "--no-recurse-submodules",
                "origin",
                "HEAD",
            ],
        )
        .unwrap();
        git(&dest, &["checkout", "--quiet", "--detach", "FETCH_HEAD"]).unwrap();
        assert!(!marker.exists(), "a hook, filter, or fsmonitor ran");
        assert!(
            !dest.join("evil").exists(),
            "submodule recursion cloned a URL that never passed validation"
        );
        let body = fs::read_to_string(dest.join("a.txt")).unwrap();
        assert!(body.contains("hello"), "{body}");
        assert!(!tmp.path().join("praxi-no-hooks").exists());
    }
}
