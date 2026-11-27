//! Saving a mail attachment: a safe file name, what the bytes really are, and a
//! write that never overwrites and never leaves the folder it was given.
//!
//! The name, the declared type and the bytes all come from a stranger. The
//! declared file name only suggests the name of the saved file (it is reduced to
//! plain characters, never used as a path); the declared type is reported next to
//! the type the bytes show, and a file that is, or looks like, a program is saved
//! with a `.bin` ending so it cannot be started by a double click. Nothing is
//! ever opened or executed here.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::EmailError;

/// Longest saved file name (bytes, extension included).
const MAX_NAME_BYTES: usize = 100;
/// Name used when the declared one leaves nothing usable.
const FALLBACK_NAME: &str = "attachment";
/// Added to a file that is, or may be taken for, a program.
const SAFE_SUFFIX: &str = ".bin";

/// Where a bot's attachments are saved. The default (no place, or `None` for a bot)
/// is the daemon's own attachment folder; a host that has per-bot workspaces
/// returns the bot's workspace folder instead.
pub trait AttachmentPlace: Send + Sync {
    /// The folder `bot` wants its attachments in; `None` = the daemon's folder.
    /// Must be an absolute directory the host owns (it is created if missing).
    fn folder_for(&self, bot: Option<&str>) -> Option<PathBuf>;

    /// May `bytes` more be written to `path` for `bot` (a workspace quota)?
    ///
    /// # Errors
    /// The reason, in a sentence, when the write must not happen.
    fn allow_write(&self, _bot: Option<&str>, _path: &Path, _bytes: u64) -> Result<(), String> {
        Ok(())
    }

    /// Called with the saved file, so the host can record where it came from.
    fn saved(&self, _path: &Path) {}
}

/// What the bytes of a file show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed {
    /// MIME type from the leading bytes (`application/octet-stream` when unknown).
    pub mime: &'static str,
    /// Customary extension of that type, without the dot.
    pub ext: &'static str,
    /// A program or script: never saved under a name that looks like a document.
    pub executable: bool,
}

const fn kind(mime: &'static str, ext: &'static str) -> Sniffed {
    Sniffed {
        mime,
        ext,
        executable: false,
    }
}

const fn program(mime: &'static str, ext: &'static str) -> Sniffed {
    Sniffed {
        mime,
        ext,
        executable: true,
    }
}

/// The type of `bytes` from their first bytes. Text is recognised only when the
/// whole sample is UTF-8 without control characters.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Sniffed {
    let head = &bytes[..bytes.len().min(512)];
    let starts = |magic: &[u8]| head.starts_with(magic);
    if starts(b"%PDF-") {
        return kind("application/pdf", "pdf");
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return kind("image/png", "png");
    }
    if starts(b"\xFF\xD8\xFF") {
        return kind("image/jpeg", "jpg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return kind("image/gif", "gif");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return kind("image/webp", "webp");
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return zip_kind(bytes);
    }
    if starts(b"\x1f\x8b") {
        return kind("application/gzip", "gz");
    }
    if starts(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return kind("application/x-ole-storage", "doc");
    }
    if starts(b"MZ") {
        return program("application/vnd.microsoft.portable-executable", "exe");
    }
    if starts(b"\x7fELF") {
        return program("application/x-executable", "elf");
    }
    if starts(b"\xCA\xFE\xBA\xBE")
        || starts(b"\xFE\xED\xFA\xCE")
        || starts(b"\xFE\xED\xFA\xCF")
        || starts(b"\xCF\xFA\xED\xFE")
        || starts(b"\xCE\xFA\xED\xFE")
    {
        return program("application/x-mach-binary", "bin");
    }
    if starts(b"#!") {
        return program("text/x-script", "sh");
    }
    if let Some(text) = text_of(head, bytes.len() > head.len()) {
        let lower = text.trim_start().to_ascii_lowercase();
        if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
            return kind("text/html", "html");
        }
        if lower.starts_with("<svg") || lower.starts_with("<?xml") && lower.contains("<svg") {
            return kind("image/svg+xml", "svg");
        }
        if lower.starts_with('{') || lower.starts_with('[') {
            return kind("application/json", "json");
        }
        return kind("text/plain", "txt");
    }
    kind("application/octet-stream", "bin")
}

/// A zip is also a Word, Excel or `PowerPoint` file, a jar or an apk: the first
/// entry names tell which (the names are stored uncompressed in the local headers).
fn zip_kind(bytes: &[u8]) -> Sniffed {
    let sample = &bytes[..bytes.len().min(4096)];
    let has = |needle: &[u8]| sample.windows(needle.len()).any(|w| w == needle);
    if has(b"word/") {
        kind(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        )
    } else if has(b"xl/") {
        kind(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        )
    } else if has(b"ppt/") {
        kind(
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "pptx",
        )
    } else if has(b"META-INF/MANIFEST.MF") || has(b"AndroidManifest.xml") {
        program("application/java-archive", "jar")
    } else {
        kind("application/zip", "zip")
    }
}

/// The sample as text, when it is text. A multi-byte character cut by the end of
/// the sample is allowed only when more bytes follow it.
fn text_of(head: &[u8], more_follows: bool) -> Option<&str> {
    let text = match std::str::from_utf8(head) {
        Ok(t) => t,
        Err(e) if more_follows && e.error_len().is_none() => {
            std::str::from_utf8(&head[..e.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    (!text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
    .then_some(text)
}

/// Extensions of files an operating system starts, opens with a script host or
/// installs. A file with one of these is saved with [`SAFE_SUFFIX`] added.
const RISKY_EXTENSIONS: [&str; 34] = [
    "exe", "com", "scr", "pif", "bat", "cmd", "msi", "msp", "dll", "sys", "lnk", "url", "js",
    "jse", "vbs", "vbe", "wsf", "wsh", "ps1", "psm1", "sh", "bash", "zsh", "jar", "hta", "cpl",
    "reg", "app", "command", "desktop", "apk", "deb", "rpm", "dmg",
];

/// Names Windows reserves for devices (with or without an extension).
const RESERVED_STEMS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn is_risky(ext: &str) -> bool {
    RISKY_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// The saved file's name: the last component of the declared one, reduced to
/// letters, digits, `.`, `_`, `-` and single spaces (anything else becomes `_`),
/// without a leading dot, a device name or an overlong stem, with an extension that
/// fits what the bytes are. A program, and a file named like one, gets `.bin` added.
#[must_use]
pub fn safe_file_name(declared: Option<&str>, sniffed: &Sniffed) -> String {
    let last = declared
        .and_then(|d| d.rsplit(['/', '\\']).next())
        .unwrap_or_default();
    let mut clean = String::new();
    let mut prev_space = false;
    for c in last.chars() {
        let ok = c.is_alphanumeric() && !c.is_control() || matches!(c, '.' | '_' | '-');
        if ok {
            clean.push(c);
            prev_space = false;
        } else if c == ' ' {
            if !prev_space {
                clean.push(' ');
            }
            prev_space = true;
        } else if !c.is_control() && !is_invisible(c) {
            clean.push('_');
            prev_space = false;
        }
    }
    let clean = clean.trim_matches(|c: char| c == '.' || c == ' ' || c == '_');
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() && e.len() <= 10 => (s, Some(e)),
        _ => (clean, None),
    };
    let stem = stem.trim_matches(|c: char| c == '.' || c == ' ');
    let stem = if stem.is_empty() { FALLBACK_NAME } else { stem };
    let stem = if RESERVED_STEMS.contains(&stem.to_ascii_lowercase().as_str()) {
        format!("{stem}_")
    } else {
        stem.to_owned()
    };
    // A file with no extension, or whose bytes are a program, gets the extension of its type.
    let ext = ext
        .map(str::to_owned)
        .or_else(|| (sniffed.ext != "bin" && !sniffed.executable).then(|| sniffed.ext.to_owned()));
    let risky = sniffed.executable || ext.as_deref().is_some_and(is_risky);
    let mut name = match &ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem,
    };
    if risky {
        name.push_str(SAFE_SUFFIX);
    }
    cut_to_bytes(&name, MAX_NAME_BYTES)
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// `name` cut to at most `max` bytes, keeping its extension (the part after the last dot).
fn cut_to_bytes(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let (stem, ext) = name.rsplit_once('.').map_or((name, ""), |(s, e)| (s, e));
    let ext_len = if ext.is_empty() { 0 } else { ext.len() + 1 };
    let mut stem_end = max.saturating_sub(ext_len).max(1);
    while !stem.is_char_boundary(stem_end) {
        stem_end -= 1;
    }
    if ext.is_empty() {
        stem[..stem_end].to_owned()
    } else {
        format!("{}.{ext}", &stem[..stem_end])
    }
}

/// Write `bytes` into `dir` under `name` (made unique when taken), never replacing a
/// file and never following a link: the file is created exclusively, with no
/// execute permission. Returns the path written.
///
/// # Errors
/// [`EmailError::Config`] when the folder or the file cannot be created.
pub fn write_unique(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, EmailError> {
    let fail = || EmailError::Config("cannot save the attachment".into());
    std::fs::create_dir_all(dir).map_err(|_| fail())?;
    let (stem, ext) = name
        .rsplit_once('.')
        .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
    for n in 0..1000u32 {
        let candidate = if n == 0 {
            name.to_owned()
        } else {
            format!("{stem}-{n}{ext}")
        };
        let path = dir.join(&candidate);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                if file.write_all(bytes).and_then(|()| file.flush()).is_err() {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(fail());
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(fail()),
        }
    }
    Err(fail())
}

/// Whether the declared type and the sniffed one disagree in a way that matters:
/// the bytes are a program or an archive or a page, or a different family from
/// what the sender said. A generic declared type (`application/octet-stream`) never
/// conflicts, and text subtypes agree with plain text.
#[must_use]
pub fn type_conflict(declared: Option<&str>, sniffed: &Sniffed) -> bool {
    let Some(declared) = declared else {
        return false;
    };
    let declared = declared
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if declared.is_empty() || declared == "application/octet-stream" || declared == sniffed.mime {
        return false;
    }
    if sniffed.executable {
        return true;
    }
    if sniffed.mime == "text/plain" {
        // Plain text is every text format and many data formats (csv, json, xml, ics ...).
        return !(declared.starts_with("text/")
            || declared.ends_with("+xml")
            || declared.ends_with("+json")
            || matches!(
                declared.as_str(),
                "application/json" | "application/xml" | "application/csv" | "message/rfc822"
            ));
    }
    // A zip family file is declared as the specific type or as a generic zip.
    if sniffed.mime == "application/zip" {
        return !(declared.contains("zip") || declared.contains("compressed"));
    }
    if sniffed.mime == "application/x-ole-storage" {
        return !(declared.contains("msword")
            || declared.contains("ms-excel")
            || declared.contains("ms-powerpoint")
            || declared.contains("ms-outlook"));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(declared: &str, bytes: &[u8]) -> String {
        safe_file_name(Some(declared), &sniff(bytes))
    }

    #[test]
    fn the_bytes_say_what_a_file_is() {
        assert_eq!(sniff(b"%PDF-1.7 ...").mime, "application/pdf");
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....").mime, "image/png");
        assert_eq!(
            sniff(b"MZ\x90\x00").mime,
            "application/vnd.microsoft.portable-executable"
        );
        assert!(sniff(b"MZ\x90\x00").executable);
        assert!(sniff(b"\x7fELF\x02").executable);
        assert!(sniff(b"#!/bin/sh\nrm -rf x").executable);
        assert_eq!(sniff(b"<!DOCTYPE html><p>x</p>").mime, "text/html");
        assert_eq!(sniff(b"hello, a plain note\n").mime, "text/plain");
        assert_eq!(sniff(b"{\"a\":1}").mime, "application/json");
        assert_eq!(
            sniff(&[0, 1, 2, 3, 200, 201]).mime,
            "application/octet-stream"
        );
        let docx = [b"PK\x03\x04".as_slice(), &[0; 26], b"word/document.xml"].concat();
        assert!(sniff(&docx).mime.ends_with("wordprocessingml.document"));
        let jar = [b"PK\x03\x04".as_slice(), &[0; 26], b"META-INF/MANIFEST.MF"].concat();
        assert!(sniff(&jar).executable);
    }

    #[test]
    fn a_multibyte_character_cut_by_the_sample_is_still_text() {
        let mut bytes = "a".repeat(511).into_bytes();
        bytes.extend("ö".as_bytes());
        bytes.extend(b" and more text");
        assert_eq!(sniff(&bytes).mime, "text/plain");
    }

    #[test]
    fn a_name_is_never_a_path() {
        assert_eq!(name("../../etc/passwd", b"text"), "passwd.txt");
        assert_eq!(name("..\\..\\win\\system.ini", b"text"), "system.ini");
        assert_eq!(name("/abs/olute/report.pdf", b"%PDF-1"), "report.pdf");
        assert_eq!(name("....", b"text"), "attachment.txt");
        assert_eq!(name("", b"%PDF-1"), "attachment.pdf");
        assert_eq!(name(".hidden", b"text"), "hidden.txt");
        assert_eq!(name("a/b/", b"text"), "attachment.txt");
    }

    #[test]
    fn odd_characters_become_underscores_and_control_marks_vanish() {
        assert_eq!(name("my file (1).pdf", b"%PDF-"), "my file _1_.pdf");
        assert_eq!(name("a\u{202E}fdp.exe", b"%PDF-"), "afdp.exe.bin");
        assert_eq!(name("tab\tname\n.txt", b"text"), "tabname.txt");
        assert_eq!(name("rapor özeti.docx", b"x"), "rapor özeti.docx");
        assert_eq!(name("a:b*c?.txt", b"text"), "a_b_c_.txt");
    }

    #[test]
    fn a_program_or_a_program_name_never_keeps_its_own_ending() {
        assert_eq!(name("invoice.pdf", b"MZ\x90\x00"), "invoice.pdf.bin");
        assert_eq!(name("setup.exe", b"%PDF-1"), "setup.exe.bin");
        assert_eq!(name("run.sh", b"echo"), "run.sh.bin");
        assert_eq!(name("noext", b"\x7fELF"), "noext.bin");
        assert_eq!(name("macro.JS", b"var a"), "macro.JS.bin");
        assert_eq!(name("photo.png", b"\x89PNG\r\n\x1a\n"), "photo.png");
    }

    #[test]
    fn device_names_and_long_names_are_handled() {
        assert_eq!(name("CON.txt", b"text"), "CON_.txt");
        assert_eq!(name("nul", b"text"), "nul_.txt");
        let long = format!("{}.pdf", "x".repeat(300));
        let cut = name(&long, b"%PDF-");
        assert!(
            cut.len() <= MAX_NAME_BYTES && cut.rsplit('.').next() == Some("pdf"),
            "{cut}"
        );
        let wide = format!("{}.txt", "ö".repeat(200));
        let cut = name(&wide, b"text");
        assert!(
            cut.len() <= MAX_NAME_BYTES && cut.rsplit('.').next() == Some("txt"),
            "{cut}"
        );
    }

    #[test]
    fn a_declared_type_that_hides_what_the_bytes_are_is_a_conflict() {
        let exe = sniff(b"MZ\x90");
        assert!(type_conflict(Some("application/pdf"), &exe));
        assert!(type_conflict(Some("image/png"), &sniff(b"%PDF-1")));
        assert!(!type_conflict(Some("application/pdf"), &sniff(b"%PDF-1")));
        assert!(!type_conflict(Some("application/octet-stream"), &exe));
        assert!(!type_conflict(None, &exe));
        assert!(!type_conflict(
            Some("text/csv; charset=utf-8"),
            &sniff(b"a,b\n1,2\n")
        ));
        assert!(type_conflict(
            Some("application/pdf"),
            &sniff(b"just words")
        ));
        let xlsx = [b"PK\x03\x04".as_slice(), &[0; 26], b"xl/workbook.xml"].concat();
        assert!(!type_conflict(
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            &sniff(&xlsx)
        ));
    }

    #[test]
    fn a_write_never_replaces_a_file_and_leaves_no_execute_bit() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("acct");
        let a = write_unique(&sub, "a.txt", b"one").unwrap();
        let b = write_unique(&sub, "a.txt", b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert_eq!(b.file_name().unwrap(), "a-1.txt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&a).unwrap().permissions().mode() & 0o111,
                0
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret.txt");
        std::fs::write(&target, b"keep").unwrap();
        let folder = dir.path().join("in");
        std::fs::create_dir_all(&folder).unwrap();
        std::os::unix::fs::symlink(&target, folder.join("a.txt")).unwrap();
        let saved = write_unique(&folder, "a.txt", b"stranger").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        assert_eq!(saved.file_name().unwrap(), "a-1.txt");
    }
}
