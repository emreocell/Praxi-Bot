//! `desktop_open`: open a web address or a file of the bot's own folders in the computer's
//! default application, or in a browser the model names.
//!
//! The job `desktop_launch` is too broad for: "open the page I just wrote in a browser". The
//! model gives an address or a path, never a program line, so the pieces that make launching
//! dangerous are fixed here:
//!
//! * an address must be `http` or `https` without a user name or password (no `file:`,
//!   `javascript:`, `data:` or app URI);
//! * a file is vetted exactly like an `fs_read` of it ([`crate::files::FileGate`]: the bot's
//!   workspace and the folders the owner allowed, never credentials, daemon data or another
//!   bot's folder, links resolved) and must be of a document type from [`OPEN_EXTENSIONS`]:
//!   programs, scripts, shortcuts, installers and macro documents are never opened, whatever
//!   the default application for them would do;
//! * a file is opened only on the computer this daemon runs on (the vetting is about this
//!   computer's folders); on a paired computer only addresses are opened;
//! * the program is the system's opener or one of four named browsers, never a model string.
//!
//! The decision whether to ask is the loop's (a Full-level owner run opens without a card
//! unless the run holds private data and read untrusted content; see `agent-core`).

use std::path::{Path, PathBuf};

use desktop_api::snapshot::AppSpec;
use serde::Deserialize;
use serde_json::Value;
use tool_registry::ToolError;

use crate::util::exec_err;

/// Longest address accepted, in bytes.
const MAX_URL_BYTES: usize = 2048;
/// Largest file opened, in bytes (a video can be large; a runaway file is not opened).
pub(crate) const MAX_OPEN_BYTES: u64 = 1 << 30;

/// File types that can be opened: documents, pages, pictures, sound and video. Everything else
/// (programs, scripts, shortcuts, installers, registry files, macro-enabled documents and
/// archives) is refused: the system's opener would RUN it.
pub(crate) const OPEN_EXTENSIONS: [&str; 33] = [
    "html", "htm", "xhtml", "svg", "txt", "md", "csv", "tsv", "json", "log", "pdf", "png", "jpg",
    "jpeg", "gif", "webp", "bmp", "ico", "mp3", "wav", "ogg", "m4a", "flac", "mp4", "webm", "mkv",
    "mov", "docx", "xlsx", "pptx", "odt", "ods", "odp",
];

/// The programs a call may name.
pub(crate) const BROWSERS: [&str; 4] = ["chrome", "chromium", "firefox", "edge"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenIn {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub with: Option<String>,
}

/// What a call asks to open, checked as far as it can be without touching the disk.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Url(String),
    /// The path as the model wrote it (vetted by the caller).
    File(String),
}

/// Which program opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum With {
    /// The system's default application for the address or file type.
    Default,
    Browser(&'static str),
}

/// Parse and check the shape of a call.
///
/// # Errors
/// Both or neither of `url` and `path`, an address that is not plain `http(s)`, an unknown
/// browser name, or a file type that is not on [`OPEN_EXTENSIONS`].
pub(crate) fn parse(input: &Value) -> Result<(Target, With), ToolError> {
    let i: OpenIn = serde_json::from_value(input.clone())
        .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
    let with = match i.with.as_deref().map(str::trim) {
        None | Some("" | "default") => With::Default,
        Some(name) => With::Browser(
            BROWSERS
                .iter()
                .copied()
                .find(|b| b.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    ToolError::InvalidInput(format!(
                        "`with` must be \"default\" or one of {}",
                        BROWSERS.join(", ")
                    ))
                })?,
        ),
    };
    let target = match (i.url, i.path) {
        (Some(url), None) => Target::Url(check_url(&url)?),
        (None, Some(path)) => {
            let path = path.trim().to_owned();
            if path.is_empty() || path.chars().any(char::is_control) {
                return Err(ToolError::InvalidInput(
                    "`path` must be a file path in one line".into(),
                ));
            }
            if !extension_ok(Path::new(&path)) {
                return Err(not_openable(&path));
            }
            Target::File(path)
        }
        _ => {
            return Err(ToolError::InvalidInput(
                "give exactly one of `url` (a web address) or `path` (a file)".into(),
            ));
        }
    };
    Ok((target, with))
}

fn check_url(raw: &str) -> Result<String, ToolError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_BYTES || raw.chars().any(char::is_control) {
        return Err(ToolError::InvalidInput(
            "`url` must be a web address in one line".into(),
        ));
    }
    let url = url::Url::parse(raw)
        .map_err(|_| ToolError::InvalidInput("`url` is not a web address".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(exec_err(
            "only http and https addresses are opened; to open a file of yours give `path`",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(exec_err(
            "an address with a user name or password is not opened",
        ));
    }
    Ok(url.into())
}

pub(crate) fn extension_ok(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| OPEN_EXTENSIONS.iter().any(|ok| ok.eq_ignore_ascii_case(e)))
}

pub(crate) fn not_openable(path: &str) -> ToolError {
    exec_err(format!(
        "{path}: this file type is not opened (only documents, pages, pictures, sound and video: {}). A program or script would run, not open",
        OPEN_EXTENSIONS.join(", ")
    ))
}

/// The system's file manager on Windows, by its full path: `explorer.exe` given an address or a
/// file path as its one argument opens it with the default application. Never `cmd`, never the
/// target itself as the program (the shell then parses it: verbs, options, a program name).
fn windows_opener() -> String {
    let root = std::env::var("SystemRoot")
        .ok()
        .filter(|r| r.len() > 3 && r.as_bytes()[1] == b':' && r.as_bytes()[2] == b'\\')
        .unwrap_or_else(|| r"C:\Windows".to_owned());
    format!(r"{}\explorer.exe", root.trim_end_matches('\\'))
}

/// Refuse a target a program could take for something other than the one thing to open: an
/// option (a leading `-`, and on Windows `/`), a leading space, and on Windows a path that names
/// another scheme or stream (`shell:`, `ms-settings:`, `file.html:stream`) or a network share
/// (`\\host\share`, which sends the owner's credentials). An address is `http(s)` by
/// [`parse`] already.
fn check_argument(platform: &str, target: &str) -> Result<(), ToolError> {
    let bad = |why: &str| Err(exec_err(format!("not opened: the target {why}")));
    if target.is_empty() || target.starts_with(char::is_whitespace) {
        return bad("is empty or starts with a space");
    }
    if target.starts_with('-') {
        return bad("starts with `-`, which a program would read as an option");
    }
    if platform == "windows" {
        let lower = target.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Ok(());
        }
        if target.starts_with(['/', '\\']) {
            return bad("starts with `/` or `\\`, which is an option or a network path here");
        }
        let b = target.as_bytes();
        let drive = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        if !drive || target[2..].contains(':') {
            return bad("is not a plain path on a drive (`C:\\folder\\file.html`)");
        }
    }
    Ok(())
}

/// The programs to try, in order, to open `target` on `platform` (`windows`, `linux`, `macos`).
/// The first one the system can start wins; all of them are fixed names, `target` is only ever
/// ONE argument (never the program, never an option: [`check_argument`]).
///
/// # Errors
/// The platform has no opener, or the target could be read as an option or another scheme.
pub(crate) fn candidates(
    platform: &str,
    target: &str,
    with: With,
) -> Result<Vec<AppSpec>, ToolError> {
    check_argument(platform, target)?;
    let app = |exe: &str, args: &[&str]| AppSpec {
        executable: exe.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    };
    let with_target = |exe: &str, lead: &[&str]| {
        let mut a = app(exe, lead);
        a.args.push(target.to_owned());
        a
    };
    Ok(match (platform, with) {
        // The file manager opens an address or a file with whatever the system associates with it.
        ("windows", With::Default) => vec![with_target(&windows_opener(), &[])],
        ("windows", With::Browser(b)) => vec![with_target(
            match b {
                "chrome" | "chromium" => "chrome.exe",
                "firefox" => "firefox.exe",
                _ => "msedge.exe",
            },
            &[],
        )],
        ("linux", With::Default) => {
            vec![with_target("xdg-open", &[]), with_target("gio", &["open"])]
        }
        ("linux", With::Browser(b)) => match b {
            "chrome" => vec![
                with_target("google-chrome", &[]),
                with_target("google-chrome-stable", &[]),
            ],
            "chromium" => vec![
                with_target("chromium", &[]),
                with_target("chromium-browser", &[]),
            ],
            "firefox" => vec![with_target("firefox", &[])],
            _ => vec![
                with_target("microsoft-edge", &[]),
                with_target("microsoft-edge-stable", &[]),
            ],
        },
        ("macos", With::Default) => vec![with_target("open", &[])],
        ("macos", With::Browser(b)) => vec![with_target(
            "open",
            &[
                "-a",
                match b {
                    "chrome" => "Google Chrome",
                    "chromium" => "Chromium",
                    "firefox" => "Firefox",
                    _ => "Microsoft Edge",
                },
            ],
        )],
        _ => {
            return Err(exec_err(format!(
                "opening files and addresses is not supported on this computer ({platform})"
            )));
        }
    })
}

/// The path to hand to the system: links are resolved by the vetting already; a Windows
/// verbatim prefix is removed because `ShellExecute` does not take it.
pub(crate) fn display_path(vetted: &Path) -> PathBuf {
    browser_cdp::plain_path(vetted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target(v: &Value) -> Result<(Target, With), String> {
        parse(v).map_err(|e| e.to_string())
    }

    #[test]
    fn an_address_must_be_plain_http_or_https() {
        let (t, w) = target(&json!({"url": " https://example.com/a?b=1 "})).unwrap();
        assert_eq!(t, Target::Url("https://example.com/a?b=1".into()));
        assert_eq!(w, With::Default);
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>1</script>",
            "ms-settings:display",
            "ftp://example.com/",
            "https://user:pw@example.com/",
            "https://exa\nmple.com/",
            "not a url",
            "",
        ] {
            assert!(target(&json!({ "url": bad })).is_err(), "{bad}");
        }
    }

    #[test]
    fn exactly_one_target_and_a_known_browser() {
        assert!(target(&json!({})).is_err());
        assert!(target(&json!({"url": "https://a.example/", "path": "a.html"})).is_err());
        assert!(target(&json!({"path": "a.html", "extra": 1})).is_err());
        assert!(target(&json!({"path": "a.html", "with": "notepad"})).is_err());
        let (_, w) = target(&json!({"path": "a.html", "with": "Firefox"})).unwrap();
        assert_eq!(w, With::Browser("firefox"));
        let (_, w) = target(&json!({"path": "a.html", "with": "default"})).unwrap();
        assert_eq!(w, With::Default);
    }

    #[test]
    fn a_file_must_be_a_document_never_a_program() {
        for ok in ["kralinho.html", "a/b/c.PDF", "x.docx", "pic.jpeg"] {
            assert!(target(&json!({ "path": ok })).is_ok(), "{ok}");
        }
        for bad in [
            "run.exe",
            "run.bat",
            "x.cmd",
            "x.ps1",
            "x.sh",
            "x.lnk",
            "x.url",
            "x.msi",
            "x.reg",
            "x.js",
            "x.vbs",
            "x.hta",
            "x.jar",
            "x.docm",
            "x.xlsm",
            "x.zip",
            "x.desktop",
            "noextension",
            "x.html.exe",
            "",
        ] {
            assert!(target(&json!({ "path": bad })).is_err(), "{bad}");
        }
        assert!(target(&json!({"path": "a\nb.html"})).is_err());
    }

    #[test]
    fn the_program_is_a_fixed_name_and_the_target_only_an_argument() {
        let url = "https://example.com/";
        let linux = candidates("linux", url, With::Default).unwrap();
        assert_eq!(linux[0].executable, "xdg-open");
        assert_eq!(linux[0].args, [url]);
        let chrome = candidates("linux", url, With::Browser("chrome")).unwrap();
        assert_eq!(chrome[0].executable, "google-chrome");
        assert_eq!(chrome[1].executable, "google-chrome-stable");
        assert!(chrome.iter().all(|c| c.args == [url]));
        let win = candidates("windows", url, With::Default).unwrap();
        assert!(
            win[0]
                .executable
                .to_ascii_lowercase()
                .ends_with(r"\explorer.exe"),
            "{}",
            win[0].executable
        );
        assert_eq!(win[0].args, [url]);
        let file = candidates("windows", r"C:\Users\Jo Doe\a.html", With::Default).unwrap();
        assert_eq!(file[0].args, [r"C:\Users\Jo Doe\a.html"]);
        let edge = candidates("windows", url, With::Browser("edge")).unwrap();
        assert_eq!(edge[0].executable, "msedge.exe");
        let mac = candidates("macos", url, With::Browser("firefox")).unwrap();
        assert_eq!(mac[0].args, ["-a", "Firefox", url]);
        assert!(candidates("plan9", url, With::Default).is_err());
    }

    /// Whatever the model writes, the target is one argument and never the program: nothing that
    /// reads as an option, a verb, another scheme or a network share is handed to an opener.
    #[test]
    fn a_target_that_could_be_an_option_or_another_scheme_is_never_handed_over() {
        for bad in [
            "-a Calculator",
            "--new-window",
            " https://example.com/",
            "\tx.html",
            "",
        ] {
            for platform in ["windows", "linux", "macos"] {
                for with in [With::Default, With::Browser("chrome")] {
                    assert!(
                        candidates(platform, bad, with).is_err(),
                        "{platform} {bad:?}"
                    );
                }
            }
        }
        for bad in [
            "/e,C:\\Windows",
            "/select,C:\\a.exe",
            "\\\\evil\\share\\a.html",
            "shell:startup",
            "ms-settings:display",
            "file:///C:/a.html",
            "javascript:alert(1)",
            "C:\\a.html:stream",
            "a.html",
            "C:a.html",
        ] {
            for with in [With::Default, With::Browser("edge")] {
                assert!(candidates("windows", bad, with).is_err(), "windows {bad:?}");
            }
        }
        // an option or a share is refused for that very reason, not only as "not a drive path"
        for bad in [
            "/e,C:\\Windows",
            "/select,C:\\a.exe",
            "\\\\evil\\share\\a.html",
        ] {
            let e = candidates("windows", bad, With::Default)
                .unwrap_err()
                .to_string();
            assert!(e.contains("option or a network path"), "{bad}: {e}");
        }
        // an absolute path is fine on the other systems, an address on all
        assert!(candidates("linux", "/home/a/x.html", With::Default).is_ok());
        assert!(candidates("macos", "/Users/a/x.html", With::Default).is_ok());
        assert!(candidates("windows", "HTTPS://example.com/", With::Default).is_ok());
    }
}
