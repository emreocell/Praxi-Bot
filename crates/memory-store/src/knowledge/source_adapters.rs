//! Bounded, non-network source adapters. Authorization belongs to the caller;
//! file access stays beneath an opened directory capability and never follows
//! a symlink in an authorized path. Extracted text remains untrusted data.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};

use super::{DocumentFormat, DocumentInput};
use crate::{MemoryError, Result};

/// Input bytes per document. Parsing runs on a bounded blocking worker.
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
/// Extracted UTF-8 bytes admitted to an index.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Maximum files and directories in a selected folder (including ignored files).
pub const MAX_SOURCE_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 24;

fn invalid(message: &str) -> MemoryError {
    MemoryError::Invalid(message.to_owned())
}

/// Open an absolute ordinary directory one component at a time. UNC/device
/// prefixes, parent traversal and symlink/reparse components are rejected.
///
/// # Errors
/// Invalid path, missing directory, or a symlink component.
pub fn plain_directory(path: &Path) -> Result<Dir> {
    if !path.is_absolute() || path.to_string_lossy().chars().any(char::is_control) {
        return Err(invalid("choose an absolute local directory"));
    }
    let mut anchor = PathBuf::new();
    let mut names = Vec::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => {
                if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                    return Err(invalid("UNC and device paths are not knowledge sources"));
                }
                anchor.push(prefix.as_os_str());
            }
            Component::RootDir => anchor.push(part.as_os_str()),
            Component::Normal(name) => names.push(name),
            Component::CurDir | Component::ParentDir => {
                return Err(invalid("path traversal is not allowed"));
            }
        }
    }
    let mut dir = Dir::open_ambient_dir(anchor, cap_std::ambient_authority())?;
    for name in names {
        dir = dir.open_dir_nofollow(name)?;
    }
    Ok(dir)
}

/// Open the canonical data directory selected at host startup. Only its root
/// is trusted; every subsequent directory entry is still opened without links.
///
/// # Errors
/// The canonical local root is missing or has acquired a symlink component.
pub fn trusted_directory(path: &Path) -> Result<Dir> {
    plain_directory(&normal_local_prefix(path)?)
}

fn normal_local_prefix(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("trusted knowledge root must be absolute"));
    }
    #[cfg(windows)]
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        if let std::path::Prefix::VerbatimDisk(drive) = prefix.kind() {
            let mut normalized = PathBuf::from(format!("{}:\\", char::from(drive)));
            for part in path.components().skip(2) {
                normalized.push(part.as_os_str());
            }
            return Ok(normalized);
        }
        if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
            return Err(invalid("knowledge paths must be on a local disk"));
        }
    }
    Ok(path.to_owned())
}

fn canonical_source_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.to_string_lossy().chars().any(char::is_control)
        || path.components().any(|part| match part {
            Component::Prefix(prefix) => !matches!(prefix.kind(), std::path::Prefix::Disk(_)),
            Component::CurDir | Component::ParentDir => true,
            _ => false,
        })
    {
        return Err(invalid("choose an absolute local path without traversal"));
    }
    // Windows canonicalize uses the final handle path, expanding 8.3 aliases.
    // Authorization still opens the original components with no-follow below.
    normal_local_prefix(&std::fs::canonicalize(path)?)
}

fn protected_path(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| {
            if let Component::Normal(name) = part {
                Some(name.to_string_lossy().to_lowercase())
            } else {
                None
            }
        })
        .collect();
    parts.iter().any(|name| {
        matches!(
            name.as_str(),
            ".ssh"
                | ".aws"
                | ".gnupg"
                | ".azure"
                | ".kube"
                | ".docker"
                | ".mozilla"
                | "vault"
                | "secrets"
                | "gcloud"
                | "google-chrome"
                | "google-chrome-beta"
                | "google-chrome-unstable"
                | "chromium"
                | "chromium-browser"
                | "brave-browser"
                | "microsoft-edge"
                | "microsoft-edge-beta"
                | "vivaldi"
                | "opera"
                | "opera software"
                | "firefox"
                | "safari"
                | "thunderbird"
                | "browser"
                | "browsers"
                | "browser-profiles"
                | "browser_profiles"
        )
    }) || parts.windows(2).any(|pair| {
        matches!(
            (pair[0].as_str(), pair[1].as_str()),
            ("google", "chrome" | "chrome beta" | "chrome sxs")
                | ("microsoft", "edge" | "edge beta" | "edge sxs")
                | ("mozilla", "firefox")
                | ("bravesoftware", "brave-browser")
                | (".config", "gh" | "hub" | "op" | "1password")
        )
    })
}

fn path_within(path: &Path, parent: &Path) -> bool {
    #[cfg(windows)]
    {
        let path: Vec<_> = path
            .components()
            .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        let parent: Vec<_> = parent
            .components()
            .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        path.starts_with(&parent)
    }
    #[cfg(not(windows))]
    path.starts_with(parent)
}

fn safe_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    let json = std::path::Path::new(&lowered)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with(['.', ' '])
        && !name.contains(['\\', ':', '\0'])
        && !name.chars().any(char::is_control)
        && !(json && (lowered.contains("credentials") || lowered.starts_with("client_secret")))
        && !matches!(
            lowered.as_str(),
            "api-token"
                | "owner-key"
                | "praxi.env"
                | "praxi-node.env"
                | "secrets.enc"
                | "identity.key"
                | "devices.json"
                | "peer-node"
                | "providers.toml"
                | "email.toml"
                | "mcp.toml"
                | "policy.json"
                | "vault"
                | "secrets"
                | "application_default_credentials.json"
                | "credentials.json"
                | "credentials.toml"
                | "secrets.json"
                | "logins.json"
                | "cookies.txt"
                | "key4.db"
                | "login data"
                | "local state"
        )
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown"
                    | "txt"
                    | "pdf"
                    | "docx"
                    | "html"
                    | "htm"
                    | "rs"
                    | "php"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "py"
                    | "go"
                    | "java"
                    | "c"
                    | "h"
                    | "cpp"
                    | "hpp"
                    | "cs"
                    | "sql"
                    | "css"
                    | "scss"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "sh"
            )
        })
}

/// A user-selected file or folder. Only names are collected; document bytes
/// are read individually. The open capability survives path replacement.
pub struct SelectedSource {
    root: Dir,
    display: PathBuf,
    files: Vec<String>,
}

impl SelectedSource {
    /// Open exactly a selected local file or folder, bounded by entry count.
    ///
    /// # Errors
    /// Unsupported file, symlink, excessive entries/depth, or inaccessible path.
    pub fn open(path: &Path, folder: bool) -> Result<Self> {
        let canonical = canonical_source_path(path)?;
        if protected_path(&canonical) {
            return Err(invalid("credential directories are not knowledge sources"));
        }
        if folder {
            let root = plain_directory(path)?;
            let mut files = Vec::new();
            let mut visited = 0;
            collect(&root, &canonical, "", &mut files, &mut visited, 0)?;
            files.sort();
            Ok(Self {
                root,
                display: canonical,
                files,
            })
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| invalid("file has no parent directory"))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| invalid("invalid file name"))?;
            let canonical_name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("invalid canonical file name"))?;
            if !safe_name(name) || !safe_name(canonical_name) || !supported(&canonical) {
                return Err(invalid("unsupported knowledge file"));
            }
            let root = plain_directory(parent)?;
            // Read through the same capability later; this check never opens data.
            let meta = root.symlink_metadata(name)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(invalid("knowledge source must be a regular file"));
            }
            Ok(Self {
                root,
                display: canonical
                    .parent()
                    .ok_or_else(|| invalid("file has no parent"))?
                    .to_owned(),
                files: vec![name.to_owned()],
            })
        }
    }

    /// Open a source for a daemon host, excluding all daemon state and any
    /// folder that contains it. The shared brain is a separate explicit grant.
    ///
    /// # Errors
    /// Unsafe source, daemon-state overlap, or an invalid shared-brain binding.
    pub fn open_on_host(
        path: &Path,
        folder: bool,
        data_dir: &Path,
        shared_brain: bool,
    ) -> Result<Self> {
        let selected = if shared_brain {
            normal_local_prefix(path)?
        } else {
            path.to_owned()
        };
        let canonical = canonical_source_path(&selected)?;
        let state = normal_local_prefix(data_dir)?;
        let brain = state.join("brain");
        let exact_brain =
            folder && path_within(&canonical, &brain) && path_within(&brain, &canonical);
        if shared_brain {
            if !exact_brain {
                return Err(invalid("shared brain must use this host's brain directory"));
            }
        } else if path_within(&canonical, &state) || path_within(&state, &canonical) {
            return Err(invalid(
                "daemon state and folders containing it are not knowledge sources",
            ));
        }
        Self::open(&selected, folder)
    }

    /// Stable, relative locators for paging and removal detection.
    #[must_use]
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// Extract one selected file. PDF/DOCX citations explicitly identify
    /// extracted text, because its line numbers are not original page offsets.
    ///
    /// # Errors
    /// Unselected locator, symlink, oversized or malformed document.
    pub fn read(&self, locator: &str) -> Result<DocumentInput> {
        if self
            .files
            .binary_search_by(|s| s.as_str().cmp(locator))
            .is_err()
        {
            return Err(invalid("file is not in this selected source"));
        }
        let mut parent = self.root.try_clone()?;
        let relative = Path::new(locator);
        if let Some(ancestors) = relative.parent() {
            for component in ancestors.components() {
                let Component::Normal(name) = component else {
                    return Err(invalid("invalid source path"));
                };
                parent = parent.open_dir_nofollow(name)?;
            }
        }
        let name = relative
            .file_name()
            .ok_or_else(|| invalid("invalid source path"))?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        // A selected regular file can be replaced by a FIFO before open.
        // Nonblocking open lets the descriptor type check reject it safely.
        #[cfg(unix)]
        {
            use cap_fs_ext::OpenOptionsSyncExt;
            options.nonblock(true);
        }
        let file = parent.open_with(name, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_SOURCE_BYTES as u64
        {
            return Err(invalid(
                "source must be a regular file without hard links and within the byte limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(invalid("source exceeds input byte limit"));
        }
        let title = self.display.join(relative).display().to_string();
        let extension = relative
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let (content, format) = extract(&extension, &title, &bytes)?;
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        Ok(DocumentInput {
            // Live exact-secret redaction can change multiline spans. All
            // citations refer to these indexed bytes, never guessed offsets
            // in the original file (including Markdown and code).
            locator: format!("{locator}#extracted-text"),
            title,
            format,
            content,
            mtime_ms,
        })
    }
}

fn collect(
    dir: &Dir,
    display: &Path,
    prefix: &str,
    files: &mut Vec<String>,
    visited: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("source folder exceeds depth limit"));
    }
    for entry in dir.entries()? {
        *visited += 1;
        if *visited > MAX_SOURCE_ENTRIES {
            return Err(invalid(
                "source folder exceeds entry limit; choose a smaller folder",
            ));
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("source has a non-UTF8 name"))?;
        if !safe_name(&name) || matches!(name.as_str(), "node_modules" | "target" | "vendor") {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let locator = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if protected_path(&display.join(&locator)) {
            continue;
        }
        if kind.is_dir() {
            let child = dir.open_dir_nofollow(&name)?;
            collect(&child, display, &locator, files, visited, depth + 1)?;
        } else if kind.is_file() && supported(Path::new(&name)) {
            files.push(locator);
        }
    }
    Ok(())
}

fn extract(extension: &str, title: &str, bytes: &[u8]) -> Result<(String, DocumentFormat)> {
    let (content, format) = match extension {
        "pdf" | "docx" => {
            let cfg = doc_tools::DocToolsConfig {
                max_input_bytes: MAX_SOURCE_BYTES as u64,
                max_preview_chars: MAX_TEXT_BYTES,
                max_rows: 2000,
                max_cell_chars: 16_000,
                max_output_bytes: MAX_TEXT_BYTES,
                ..doc_tools::DocToolsConfig::default()
            };
            let reply = doc_tools::extract_bytes(&cfg, extension, title, bytes)
                .map_err(|_| invalid("document extraction failed"))?;
            (reply.markdown, DocumentFormat::Markdown)
        }
        "html" | "htm" => {
            let html =
                std::str::from_utf8(bytes).map_err(|_| invalid("text source must be UTF-8"))?;
            let extracted =
                web_tools::html_to_markdown(html, None, &web_tools::ExtractOptions::default());
            (extracted.markdown, DocumentFormat::Markdown)
        }
        _ => {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| invalid("text source must be UTF-8"))?
                .to_owned();
            let format = match extension {
                "md" | "markdown" => DocumentFormat::Markdown,
                "txt" => DocumentFormat::PlainText,
                _ => DocumentFormat::Code,
            };
            (text, format)
        }
    };
    if content.len() > MAX_TEXT_BYTES {
        return Err(invalid("extracted text exceeds index byte limit"));
    }
    Ok((redact_source(&content), format))
}

/// Redact known credential patterns while preserving extracted line numbers.
/// This heuristic supplements source opt-ins; it cannot recognize every secret.
#[must_use]
pub fn redact_source(text: &str) -> String {
    text.split('\n')
        .map(crate::redact)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Index a user-saved page without fetching its URL or any embedded resource.
///
/// # Errors
/// Non-HTTP URL, credentials, oversized or non-UTF8 saved text.
pub fn saved_web(url: &str, content: &str, html: bool) -> Result<DocumentInput> {
    let parsed = url::Url::parse(url).map_err(|_| invalid("invalid saved page URL"))?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || url.len() > 2048
    {
        return Err(invalid(
            "saved page needs an HTTP(S) URL without credentials",
        ));
    }
    if parsed.query_pairs().any(|(key, value)| {
        let key = key.to_ascii_lowercase().replace('-', "_");
        matches!(
            key.as_str(),
            "key"
                | "api_key"
                | "apikey"
                | "access_token"
                | "refresh_token"
                | "token"
                | "auth"
                | "authorization"
                | "password"
                | "passwd"
                | "secret"
                | "client_secret"
                | "signature"
                | "sig"
                | "credential"
        ) || crate::contains_secret(&value)
    }) || parsed.fragment().is_some_and(crate::contains_secret)
    {
        return Err(invalid("saved URL contains secret-like data"));
    }
    if content.len() > MAX_SOURCE_BYTES {
        return Err(invalid("saved page exceeds byte limit"));
    }
    let (content, format) = extract(if html { "html" } else { "md" }, url, content.as_bytes())?;
    Ok(DocumentInput {
        locator: format!("{parsed}#saved-text"),
        title: parsed.to_string(),
        format,
        content,
        mtime_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_81_saved_urls_accept_long_ordinary_paths_but_reject_query_credentials() {
        assert!(
            saved_web(
                "https://example.com/blog/2024/understanding-rust-async-await-patterns",
                "A source snapshot.",
                false
            )
            .is_ok()
        );
        for url in [
            "https://example.com/?client_secret=short-secret",
            "https://example.com/?access_token=short-token",
            "https://example.com/?password=short-password",
        ] {
            assert!(
                saved_web(url, "A source snapshot.", false).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn review_81_credential_and_browser_directories_cannot_be_selected() {
        let root = tempfile::tempdir().unwrap();
        for relative in [
            ".kube",
            ".docker",
            ".config/gcloud",
            ".mozilla/firefox/profile",
            ".config/google-chrome/Default",
            "AppData/Local/Google/Chrome/User Data/Default",
            "AppData/Roaming/Mozilla/Firefox/Profiles/default",
        ] {
            let folder = root.path().join(relative);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("selected.txt"), "private credential evidence").unwrap();
            assert!(
                SelectedSource::open(&folder, true).is_err(),
                "selected credential directory: {relative}"
            );
            assert!(
                SelectedSource::open(&folder.join("selected.txt"), false).is_err(),
                "selected file in credential directory: {relative}"
            );
        }
    }

    #[test]
    fn review_81_protected_credential_files_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "application_default_credentials.json",
            "logins.json",
            "credentials.json",
            "service-account-credentials-prod.json",
            "client_secret_desktop.json",
            "credentials.toml",
            "secrets.json",
            "cookies.txt",
        ] {
            std::fs::write(root.path().join(name), r#"{"client_secret":"short"}"#).unwrap();
            assert!(
                SelectedSource::open(&root.path().join(name), false).is_err(),
                "{name}"
            );
        }
        std::fs::write(root.path().join("readme.md"), "safe selected notes").unwrap();
        assert_eq!(
            SelectedSource::open(root.path(), true).unwrap().files(),
            &["readme.md"]
        );
    }

    #[cfg(windows)]
    #[test]
    #[allow(unsafe_code)]
    fn review_81_windows_short_alias_cannot_select_a_browser_profile() {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        use windows::core::PCWSTR;

        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("browser-profiles");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("notes.txt"), "Private browser state.").unwrap();
        let wide: Vec<u16> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut short = vec![0_u16; 32_768];
        // SAFETY: the source is NUL-terminated and the destination slice owns
        // its complete writable capacity; Win32 receives both lengths.
        let count = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut short)) } as usize;
        if count == 0 || count >= short.len() {
            eprintln!(
                "SKIP review_81_windows_short_alias: volume did not provide a DOS short name ({})",
                std::io::Error::last_os_error()
            );
            return;
        }
        let short = PathBuf::from(String::from_utf16(&short[..count]).unwrap());
        if !short.to_string_lossy().contains('~') {
            eprintln!(
                "SKIP review_81_windows_short_alias: NTFS 8.3 aliases are disabled on this test volume"
            );
            return;
        }
        assert!(SelectedSource::open(&short, true).is_err());
        assert!(SelectedSource::open(&short.join("notes.txt"), false).is_err());
        eprintln!("PASS review_81_windows_short_alias: generated DOS alias was rejected");
    }

    #[test]
    fn selected_tree_is_bounded_and_extracts_structure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "# Heading\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".env"), "secret").unwrap();
        let selected = SelectedSource::open(dir.path(), true).unwrap();
        assert_eq!(selected.files(), &["notes.md"]);
        assert!(
            selected
                .read("notes.md")
                .unwrap()
                .content
                .contains("| 1 | 2 |")
        );
        assert!(selected.read("../outside.txt").is_err());
    }

    #[test]
    fn saved_html_never_fetches_and_drops_scripts() {
        let doc = saved_web(
            "https://example.invalid/private",
            "<main><h1>Title</h1><p>Saved text</p><script>bad()</script></main>",
            true,
        )
        .unwrap();
        assert!(doc.content.contains("Saved text"));
        assert!(!doc.content.contains("bad()"));
        assert!(saved_web("file:///etc/passwd", "x", false).is_err());
        assert!(saved_web("https://user:password@example.test", "x", false).is_err());
    }

    #[test]
    fn selected_hard_link_cannot_import_an_outside_file() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original.txt");
        let selected = root.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        std::fs::write(&original, "outside evidence").unwrap();
        std::fs::hard_link(&original, selected.join("linked.txt")).unwrap();
        let source = SelectedSource::open(&selected, true).unwrap();
        assert!(source.read("linked.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_fifo_is_rejected_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.txt");
        std::fs::write(&path, "ordinary document").unwrap();
        let source = SelectedSource::open(&path, false).unwrap();
        std::fs::remove_file(&path).unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(source.read("selected.txt").is_err());
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            "special file must be rejected without waiting for a writer"
        );
    }

    #[cfg(windows)]
    #[test]
    fn selected_junction_cannot_escape_capability() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let junction = root.path().join("outside-link");
        std::fs::write(root.path().join("owned.txt"), "Selected evidence.").unwrap();
        std::fs::write(outside.path().join("private.txt"), "Outside evidence.").unwrap();
        let result = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "junction fixture: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(SelectedSource::open(&junction, true).is_err());
        assert!(SelectedSource::open(&junction.join("private.txt"), false).is_err());
        let selected = SelectedSource::open(root.path(), true).unwrap();
        assert_eq!(selected.files(), &["owned.txt"]);
        assert!(selected.read("outside-link/private.txt").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("private.txt")).unwrap(),
            "Outside evidence."
        );
        std::fs::remove_dir(junction).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_file_and_directory_symlinks_cannot_escape_capability() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "safe").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "private").unwrap();
        let selected = SelectedSource::open(&dir.path().join("file.txt"), false).unwrap();
        std::fs::remove_file(dir.path().join("file.txt")).unwrap();
        symlink(
            outside.path().join("secret.txt"),
            dir.path().join("file.txt"),
        )
        .unwrap();
        assert!(selected.read("file.txt").is_err());
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(SelectedSource::open(&dir.path().join("escape"), true).is_err());
    }
}
