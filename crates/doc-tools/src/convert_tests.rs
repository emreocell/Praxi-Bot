//! Tests for `doc_convert` / `doc_preview` with fake `soffice` / `pdftoppm` scripts.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tool_registry::ToolError;

use crate::common::Reply;
use crate::convert::{ConvertConfig, run_convert, run_preview};
use crate::{DocToolsConfig, WriteGuard};

/// Write an executable fake converter (a shell script, or a `.cmd` file on Windows).
fn fake(dir: &Path, name: &str, unix: &str, win: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = win;
        let p = dir.join(name);
        fs::write(&p, unix).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
    #[cfg(windows)]
    {
        let _ = unix;
        let p = dir.join(format!("{name}.cmd"));
        fs::write(&p, win.replace('\n', "\r\n")).unwrap();
        p
    }
}

/// A fake `soffice`: logs its arguments and writes `FAKE-<ext>` plus the input as the result.
fn fake_soffice(dir: &Path, log: &Path) -> PathBuf {
    let l = log.display();
    let unix = format!(
        "#!/bin/sh\necho \"$@\" > '{l}'\nout=''; to=''; last=''\nwhile [ $# -gt 0 ]; do\n case \"$1\" in --outdir) out=\"$2\"; shift;; --convert-to) to=\"$2\"; shift;; esac\n last=\"$1\"; shift\ndone\next=\"${{to%%:*}}\"\n{{ echo FAKE-$ext; cat \"$last\"; }} > \"$out/input.$ext\"\n"
    );
    let win = format!(
        "@echo off\nsetlocal enabledelayedexpansion\necho %* > \"{l}\"\nset OUT=\nset TO=\nset LAST=\n:loop\nif \"%~1\"==\"\" goto done\nif \"%~1\"==\"--outdir\" set \"OUT=%~2\"\nif \"%~1\"==\"--convert-to\" set \"TO=%~2\"\nset \"LAST=%~1\"\nshift\ngoto loop\n:done\nfor /f \"delims=:\" %%a in (\"!TO!\") do set EXT=%%a\n(echo FAKE-!EXT!& type \"!LAST!\") > \"!OUT!\\input.!EXT!\"\n"
    );
    fake(dir, "soffice", &unix, &win)
}

/// A fake `pdftoppm` that always renders three pages named `<prefix>-N.png`.
fn fake_pdftoppm(dir: &Path) -> PathBuf {
    fake(
        dir,
        "pdftoppm",
        "#!/bin/sh\nfor a in \"$@\"; do prefix=\"$a\"; done\nfor n in 1 2 3; do echo png$n > \"${prefix}-$n.png\"; done\n",
        "@echo off\nsetlocal enabledelayedexpansion\n:loop\nif \"%~1\"==\"\" goto done\nset \"LAST=%~1\"\nshift\ngoto loop\n:done\nfor %%n in (1 2 3) do echo png%%n> \"!LAST!-%%n.png\"\n",
    )
}

/// A config whose converters are exactly the given fakes; `None` means "not installed",
/// whatever the host has.
fn cfg_with(soffice: Option<PathBuf>, ppm: Option<PathBuf>) -> DocToolsConfig {
    // These tests exercise the plumbing with fakes that live in /tmp; the real network
    // sandbox is covered by its own test below.
    crate::convert_sandbox::force_no_sandbox(true);
    DocToolsConfig {
        convert: ConvertConfig {
            soffice_path: Some(soffice.unwrap_or_else(|| PathBuf::from("/nonexistent/soffice"))),
            pdftoppm_path: Some(ppm.unwrap_or_else(|| PathBuf::from("/nonexistent/pdftoppm"))),
            timeout: Duration::from_secs(20),
            read_guard: Some(WriteGuard::new(|_| Ok(()))),
            allow_unsandboxed_convert: true,
            convert_enabled_on_windows: true,
            ..ConvertConfig::default()
        },
        ..DocToolsConfig::default()
    }
}

/// Like [`cfg_with`], with the bot workspace set to `root`.
fn cfg_in(root: &Path, soffice: Option<PathBuf>, ppm: Option<PathBuf>) -> DocToolsConfig {
    let mut cfg = cfg_with(soffice, ppm);
    cfg.convert.output_root = Some(root.to_path_buf());
    cfg
}

fn docx_bytes() -> Vec<u8> {
    crate::docx::build_docx(&[crate::docx::WBlock::Paragraph {
        text: "Offer for the customer".into(),
        bold: false,
    }])
    .unwrap()
}

fn convert(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    run_convert(cfg, input)
}

#[test]
fn docx_to_pdf_runs_soffice_in_a_private_profile_and_never_overwrites() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let src = work.path().join("offer.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let input = json!({"path": src.to_str().unwrap(), "to": "pdf"});

    let r = convert(&cfg, input.clone()).unwrap();
    assert_eq!(r.data["engine"], "libreoffice");
    let made = fs::read(work.path().join("offer.pdf")).unwrap();
    assert!(made.starts_with(b"FAKE-pdf"), "{made:?}");
    let args = fs::read_to_string(&log).unwrap();
    for needle in [
        "UserInstallation",
        "--safe-mode",
        "--headless",
        "--convert-to",
    ] {
        assert!(args.contains(needle), "{needle} missing in {args}");
    }
    // The converter never saw the original file, only a copy in its own temp folder.
    assert!(!args.contains("offer.docx"), "{args}");

    let err = convert(&cfg, input.clone()).unwrap_err();
    assert!(err.to_string().contains("overwrite"), "{err}");
    let mut again = input;
    again["overwrite"] = json!(true);
    convert(&cfg, again).unwrap();
}

#[test]
fn macro_files_are_refused_before_any_converter_runs() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    // By extension.
    let docm = work.path().join("a.docm");
    fs::write(&docm, docx_bytes()).unwrap();
    let err = convert(&cfg, json!({"path": docm.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("macro"), "{err}");
    // By content: a ".docx" that carries a VBA project.
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    zip.start_file("word/document.xml", o).unwrap();
    zip.write_all(b"<w/>").unwrap();
    zip.start_file("word/vbaProject.bin", o).unwrap();
    zip.write_all(b"x").unwrap();
    let sneaky = work.path().join("b.docx");
    fs::write(&sneaky, zip.finish().unwrap().into_inner()).unwrap();
    let err = convert(&cfg, json!({"path": sneaky.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("macros"), "{err}");
    assert!(!log.exists(), "soffice must not have been started");
}

#[test]
fn without_libreoffice_pdf_to_docx_falls_back_and_the_rest_says_install_the_office_pack() {
    let work = tempfile::tempdir().unwrap();
    let cfg = cfg_in(work.path(), None, None);
    let pdf = work.path().join("in.pdf");
    fs::write(&pdf, crate::pdf::tests::make_pdf(&[&["Quarterly numbers"]])).unwrap();
    let r = convert(&cfg, json!({"path": pdf.to_str().unwrap(), "to": "docx"})).unwrap();
    assert_eq!(r.data["engine"], "built-in");
    let made = fs::read(work.path().join("in.docx")).unwrap();
    let text = crate::docx::read_bytes(&cfg, "x", &made).unwrap().markdown;
    assert!(text.contains("Quarterly numbers"), "{text}");

    let docx = work.path().join("o.docx");
    fs::write(&docx, docx_bytes()).unwrap();
    let err = convert(&cfg, json!({"path": docx.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("office pack"), "{err}");
}

#[test]
fn a_hung_converter_is_stopped_at_the_timeout() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let hang = fake(
        tools.path(),
        "soffice",
        "#!/bin/sh\nsleep 30\n",
        "@echo off\nping -n 30 127.0.0.1 >nul\n",
    );
    let mut cfg = cfg_in(work.path(), Some(hang), None);
    cfg.convert.timeout = Duration::from_secs(1);
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let started = Instant::now();
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("stopped"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(!work.path().join("a.pdf").exists());
}

#[test]
fn output_must_stay_inside_the_workspace_root() {
    let tools = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let mut cfg = cfg_in(root.path(), Some(fake_soffice(tools.path(), &log)), None);
    cfg.convert.output_root = Some(root.path().to_path_buf());
    let src = root.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let outside = elsewhere.path().join("a.pdf");
    let err = convert(
        &cfg,
        json!({"path": src.to_str().unwrap(), "to": "pdf", "output_path": outside.to_str().unwrap()}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("workspace"), "{err}");
    assert!(!outside.exists());
    let dotdot = format!("{}/../x.pdf", root.path().join("sub").display());
    assert!(
        convert(
            &cfg,
            json!({"path": src.to_str().unwrap(), "to": "pdf", "output_path": dotdot})
        )
        .is_err()
    );
    convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap();
    assert!(root.path().join("a.pdf").exists());
}

#[test]
fn preview_renders_only_the_requested_pages_and_returns_the_images() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = cfg_in(work.path(), None, Some(fake_pdftoppm(tools.path())));
    let pdf = work.path().join("doc.pdf");
    fs::write(&pdf, b"%PDF-1.4 fake").unwrap();
    let r = run_preview(&cfg, json!({"path": pdf.to_str().unwrap(), "pages": "1-2"})).unwrap();
    assert!(work.path().join("doc-p1.png").exists());
    assert!(work.path().join("doc-p2.png").exists());
    assert!(!work.path().join("doc-p3.png").exists());
    assert_eq!(r.images.len(), 2);
    // Existing previews are kept unless asked.
    let err = run_preview(&cfg, json!({"path": pdf.to_str().unwrap(), "pages": "1"})).unwrap_err();
    assert!(err.to_string().contains("overwrite"), "{err}");
    // Office input without LibreOffice says what to install.
    let docx = work.path().join("o.docx");
    fs::write(&docx, docx_bytes()).unwrap();
    let err = run_preview(&cfg, json!({"path": docx.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("office pack"), "{err}");
}

// ------------------------------------------------------------- security regressions

fn zip_with(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (name, body) in parts {
        zip.start_file(*name, o).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn try_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        } else {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }
}

#[test]
fn documents_with_external_references_are_refused_before_soffice_starts() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let rel = |target: &str, mode: &str, kind: &str| {
        format!(
            "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}\" Target=\"{target}\" {mode}/></Relationships>"
        )
    };
    let ct = "<Types/>";
    let evil: Vec<(&str, Vec<u8>)> = vec![
        (
            "linked-image.docx",
            zip_with(&[
                ("[Content_Types].xml", ct),
                ("word/document.xml", "<w/>"),
                (
                    "word/_rels/document.xml.rels",
                    &rel(
                        "http://attacker.invalid/x.png",
                        "TargetMode=\"External\"",
                        "image",
                    ),
                ),
            ]),
        ),
        (
            "unc-template.docx",
            zip_with(&[
                ("[Content_Types].xml", ct),
                ("word/document.xml", "<w/>"),
                (
                    "word/_rels/settings.xml.rels",
                    &rel(
                        "\\\\attacker\\share\\t.dotm",
                        "TargetMode = 'External'",
                        "attachedTemplate",
                    ),
                ),
            ]),
        ),
        (
            "dde.docx",
            zip_with(&[
                ("[Content_Types].xml", ct),
                (
                    "word/document.xml",
                    "<w:document><w:instrText>DDE</w:instrText><w:instrText>AUTO calc</w:instrText></w:document>",
                ),
            ]),
        ),
        (
            "linked.odt",
            zip_with(&[
                ("mimetype", "application/vnd.oasis.opendocument.text"),
                (
                    "content.xml",
                    "<office:document-content><draw:image xlink:href=\"https://attacker.invalid/p.png\"/></office:document-content>",
                ),
            ]),
        ),
        (
            "template.odt",
            zip_with(&[
                ("mimetype", "application/vnd.oasis.opendocument.text"),
                ("content.xml", "<x/>"),
                (
                    "meta.xml",
                    "<office:document-meta><meta:template xlink:href=\"file://attacker/share/t.ott\"/></office:document-meta>",
                ),
            ]),
        ),
    ];
    for (name, bytes) in evil {
        let src = work.path().join(name);
        fs::write(&src, bytes).unwrap();
        let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
        assert!(
            err.to_string().contains("not plain enough"),
            "{name}: {err}"
        );
        assert!(!log.exists(), "{name}: soffice must not have been started");
    }
    // A clean document (and an ODF file with only internal links) still converts.
    let ok = work.path().join("ok.odt");
    fs::write(
        &ok,
        zip_with(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            (
                "content.xml",
                "<x><draw:image xlink:href=\"Pictures/a.png\"/><text:a xlink:href=\"#bm\"/></x>",
            ),
        ]),
    )
    .unwrap();
    convert(&cfg, json!({"path": ok.to_str().unwrap(), "to": "pdf"})).unwrap();
    let clean = work.path().join("clean.docx");
    fs::write(&clean, docx_bytes()).unwrap();
    convert(&cfg, json!({"path": clean.to_str().unwrap(), "to": "pdf"})).unwrap();
}

#[test]
fn an_oversized_preview_file_is_refused_before_it_is_read() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let big = fake(
        tools.path(),
        "pdftoppm",
        "#!/bin/sh\nfor a in \"$@\"; do prefix=\"$a\"; done\nyes png | head -c 5000 > \"${prefix}-1.png\"\n",
        "@echo off\nsetlocal enabledelayedexpansion\n:loop\nif \"%~1\"==\"\" goto done\nset \"LAST=%~1\"\nshift\ngoto loop\n:done\nfor /l %%n in (1,1,400) do echo 0123456789abcdef>> \"!LAST!-1.png\"\n",
    );
    let mut cfg = cfg_in(work.path(), None, Some(big));
    cfg.max_output_bytes = 1000;
    let pdf = work.path().join("doc.pdf");
    fs::write(&pdf, b"%PDF-1.4 fake").unwrap();
    let err = run_preview(&cfg, json!({"path": pdf.to_str().unwrap(), "pages": "1"})).unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(!work.path().join("doc-p1.png").exists());
}

#[test]
fn an_oversized_soffice_result_and_a_bad_target_format_are_refused() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let mut cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "exe"})).unwrap_err();
    assert!(err.to_string().contains("cannot convert"), "{err}");
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "xlsx"})).unwrap_err();
    assert!(err.to_string().contains("cannot convert"), "{err}");
    assert!(!log.exists());
    cfg.max_output_bytes = 4; // the fake result is larger than this
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(!work.path().join("a.pdf").exists());
}

#[test]
fn without_an_output_root_the_converters_fail_closed() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_with(
        Some(fake_soffice(tools.path(), &log)),
        Some(fake_pdftoppm(tools.path())),
    );
    assert!(cfg.convert.output_root.is_none());
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("workspace"), "{err}");
    assert!(!log.exists());
    assert!(!work.path().join("a.pdf").exists());
    let pdf = work.path().join("d.pdf");
    fs::write(&pdf, b"%PDF-1.4 fake").unwrap();
    let err = run_preview(&cfg, json!({"path": pdf.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("workspace"), "{err}");
    assert!(!work.path().join("d-p1.png").exists());
}

#[test]
fn a_symlinked_input_is_refused() {
    let tools = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let secret = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let mut cfg = cfg_in(root.path(), Some(fake_soffice(tools.path(), &log)), None);
    cfg.convert.output_root = Some(root.path().to_path_buf());
    let real = secret.path().join("real.docx");
    fs::write(&real, docx_bytes()).unwrap();
    let link = root.path().join("link.docx");
    if !try_symlink(&real, &link) {
        return; // symlinks need a privilege here
    }
    let err = convert(&cfg, json!({"path": link.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("link"), "{err}");
    // A symlinked folder that leads out of the workspace is refused as an ancestor.
    let dir_link = root.path().join("out-dir");
    if try_symlink(secret.path(), &dir_link) {
        let via = dir_link.join("real.docx");
        let err = convert(&cfg, json!({"path": via.to_str().unwrap(), "to": "pdf"})).unwrap_err();
        assert!(
            err.to_string().contains("link") || err.to_string().contains("workspace"),
            "{err}"
        );
        let target = dir_link.join("made.pdf");
        let src = root.path().join("ok.docx");
        fs::write(&src, docx_bytes()).unwrap();
        let err = convert(
            &cfg,
            json!({"path": src.to_str().unwrap(), "to": "pdf", "output_path": target.to_str().unwrap()}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("workspace"), "{err}");
        assert!(!secret.path().join("made.pdf").exists());
    }
    assert!(!log.exists());
}

#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(s) => !s
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z'),
    }
}

#[cfg(target_os = "linux")]
fn wait_dead(pid: u32) -> bool {
    for _ in 0..80 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(target_os = "linux")]
#[test]
fn a_grandchild_dies_on_timeout_and_after_a_successful_run() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let pidfile = tools.path().join("pid");
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let p = pidfile.display();

    // Timeout: the converter's own child must not survive.
    let hang = fake(
        tools.path(),
        "soffice-hang",
        &format!("#!/bin/sh\nsleep 60 &\necho $! > '{p}'\nwait\n"),
        "",
    );
    let mut cfg = cfg_in(work.path(), Some(hang), None);
    cfg.convert.timeout = Duration::from_secs(1);
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("stopped"), "{err}");
    let pid: u32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(wait_dead(pid), "grandchild {pid} survived the timeout");

    // Success: a lingering helper (like soffice.bin) is cleaned up too.
    fs::remove_file(&pidfile).unwrap();
    let lingering = fake(
        tools.path(),
        "soffice-linger",
        &format!(
            "#!/bin/sh\nsleep 60 >/dev/null 2>&1 &\necho $! > '{p}'\nfor a in \"$@\"; do case \"$prev\" in --outdir) out=\"$a\";; esac; prev=\"$a\"; done\necho FAKE > \"$out/input.pdf\"\n"
        ),
        "",
    );
    let cfg = cfg_in(work.path(), Some(lingering), None);
    convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap();
    let pid: u32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_dead(pid),
        "lingering helper {pid} survived a successful run"
    );
}

#[cfg(unix)]
#[test]
fn soffice_gets_a_locked_down_profile_and_an_allow_listed_environment() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let seen = tools.path().join("seen.txt");
    let s = seen.display();
    let spy = fake(
        tools.path(),
        "soffice-spy",
        &format!(
            "#!/bin/sh\nenv > '{s}'\nfor a in \"$@\"; do case \"$a\" in -env:UserInstallation=file://*) prof=\"${{a#-env:UserInstallation=file://}}\";; esac; done\ncat \"$prof/user/registrymodifications.xcu\" >> '{s}'\nfor a in \"$@\"; do case \"$prev\" in --outdir) out=\"$a\";; esac; prev=\"$a\"; done\necho FAKE > \"$out/input.pdf\"\n"
        ),
        "",
    );
    let cfg = cfg_in(work.path(), Some(spy), None);
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap();
    let text = fs::read_to_string(&seen).unwrap();
    for line in text.lines().take_while(|l| !l.starts_with("<?xml")) {
        let key = line.split('=').next().unwrap_or("");
        assert!(
            [
                "PATH",
                "HOME",
                "USERPROFILE",
                "TMPDIR",
                "TEMP",
                "TMP",
                "LANG",
                "LC_ALL",
                "PWD",
                "SHLVL",
                "_",
                "OLDPWD"
            ]
            .contains(&key),
            "unexpected environment variable {key}"
        );
    }
    for needle in [
        "MacroSecurityLevel",
        "<value>3</value>",
        "oor:name=\"Link\"",
        "ooInetProxyType",
    ] {
        assert!(
            text.contains(needle),
            "{needle} missing in the profile:\n{text}"
        );
    }
}

/// A fake `pdftoppm` that logs its arguments (one line per call) and renders the page it
/// is asked for (`-f N`), plus a pretend extra page 1 so over-rendering would show.
fn logging_pdftoppm(dir: &Path, log: &Path) -> PathBuf {
    let l = log.display();
    fake(
        dir,
        "pdftoppm",
        &format!(
            "#!/bin/sh\necho \"$@\" >> '{l}'\nfor a in \"$@\"; do prefix=\"$a\"; done\nwhile [ $# -gt 0 ]; do case \"$1\" in -f) f=\"$2\";; esac; shift; done\necho png > \"${{prefix}}-$f.png\"\n"
        ),
        &format!(
            "@echo off\nsetlocal enabledelayedexpansion\necho %* >> \"{l}\"\nset F=1\n:loop\nif \"%~1\"==\"\" goto done\nif \"%~1\"==\"-f\" set \"F=%~2\"\nset \"LAST=%~1\"\nshift\ngoto loop\n:done\necho png> \"!LAST!-!F!.png\"\n"
        ),
    )
}

#[test]
fn preview_bounds_the_pixel_size_and_the_page_span() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("calls.txt");
    let cfg = cfg_in(
        work.path(),
        None,
        Some(logging_pdftoppm(tools.path(), &log)),
    );
    let pdf = work.path().join("doc.pdf");
    fs::write(&pdf, b"%PDF-1.4 fake").unwrap();
    // "1,100000" must not become one render of 100000 pages.
    run_preview(
        &cfg,
        json!({"path": pdf.to_str().unwrap(), "pages": "1,99999", "dpi": 200}),
    )
    .unwrap();
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("-scale-to"), "no pixel cap in: {calls}");
    for line in calls.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let value = |flag: &str| -> Option<u32> {
            words
                .iter()
                .position(|w| *w == flag)
                .and_then(|i| words.get(i + 1))
                .and_then(|v| v.parse().ok())
        };
        let (f, l) = (value("-f").unwrap(), value("-l").unwrap());
        assert!(l - f < 10, "page span too wide: {line}");
        assert!(value("-scale-to").unwrap() <= 2000, "{line}");
    }
}

#[test]
fn a_pdf_with_an_absurd_page_size_is_refused_up_front() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("calls.txt");
    let cfg = cfg_in(
        work.path(),
        None,
        Some(logging_pdftoppm(tools.path(), &log)),
    );
    let pdf = work.path().join("huge.pdf");
    fs::write(
        &pdf,
        b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [0 0 2000000 2000000] >>\nendobj\n",
    )
    .unwrap();
    let err = run_preview(&cfg, json!({"path": pdf.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("page size"), "{err}");
    assert!(!log.exists(), "pdftoppm must not have been started");
}

#[test]
fn a_converter_that_floods_its_output_folder_is_stopped_early() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let flood = fake(
        tools.path(),
        "pdftoppm",
        "#!/bin/sh\nfor a in \"$@\"; do prefix=\"$a\"; done\nyes png | head -c 200000 > \"${prefix}-1.png\"\nsleep 30\n",
        "@echo off\nsetlocal enabledelayedexpansion\n:loop\nif \"%~1\"==\"\" goto done\nset \"LAST=%~1\"\nshift\ngoto loop\n:done\nfor /l %%n in (1,1,2000) do echo 0123456789abcdef0123456789abcdef>> \"!LAST!-1.png\"\nping -n 30 127.0.0.1 >nul\n",
    );
    let mut cfg = cfg_in(work.path(), None, Some(flood));
    cfg.max_output_bytes = 1000;
    cfg.convert.timeout = Duration::from_secs(25);
    let pdf = work.path().join("doc.pdf");
    fs::write(&pdf, b"%PDF-1.4 fake").unwrap();
    let started = Instant::now();
    let err = run_preview(&cfg, json!({"path": pdf.to_str().unwrap(), "pages": "1"})).unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the flood was not stopped"
    );
}

#[test]
fn the_input_must_pass_the_read_guard_and_a_guard_is_mandatory() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let mut cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let src = work.path().join("secret.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let input = json!({"path": src.to_str().unwrap(), "to": "pdf"});
    // Same role as the `fs_read` check: a protected input is refused, nothing runs.
    cfg.convert.read_guard = Some(WriteGuard::new(|p| {
        if p.contains("secret") {
            Err("protected folder".into())
        } else {
            Ok(())
        }
    }));
    let err = convert(&cfg, input.clone()).unwrap_err();
    assert!(err.to_string().contains("protected"), "{err}");
    assert!(!log.exists());
    assert!(!work.path().join("secret.pdf").exists());
    // No guard configured: fail closed, for both tools.
    cfg.convert.read_guard = None;
    let err = convert(&cfg, input).unwrap_err();
    assert!(err.to_string().contains("guard"), "{err}");
    let err = run_preview(&cfg, json!({"path": src.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("guard"), "{err}");
    assert!(!log.exists());
}

#[test]
fn path_lookup_ignores_empty_relative_and_current_directory_entries() {
    let good = tempfile::tempdir().unwrap();
    let bin = good.path().join("soffice");
    fs::write(&bin, "x").unwrap();
    let cwd = std::env::current_dir().unwrap();
    let paths = std::env::join_paths([
        PathBuf::from(""),
        PathBuf::from("."),
        PathBuf::from("relative/bin"),
        cwd.clone(),
        good.path().to_path_buf(),
    ])
    .unwrap();
    assert_eq!(crate::convert::find_in(&paths, &["soffice"]), Some(bin));
    // A matching file only in the current directory is never picked up.
    let only_bad = std::env::join_paths([PathBuf::from(""), PathBuf::from("."), cwd]).unwrap();
    assert_eq!(
        crate::convert::find_in(&only_bad, &["Cargo.toml"]),
        None,
        "the working directory must not be searched"
    );
}

// ------------------------------------------------- design: sandbox, paths, plain-only

#[test]
fn unc_and_device_input_paths_are_refused_before_any_filesystem_call() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    for bad in [
        r"\\attacker.invalid\share\a.docx",
        "//attacker.invalid/share/a.docx",
        r"\\?\UNC\attacker.invalid\share\a.docx",
        r"\\.\pipe\x.docx",
    ] {
        let err = convert(&cfg, json!({"path": bad, "to": "pdf"})).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("cannot stat"), "{bad}: {msg}");
        assert!(msg.contains("not allowed"), "{bad}: {msg}");
        let err = run_preview(&cfg, json!({"path": bad})).unwrap_err();
        assert!(err.to_string().contains("not allowed"), "{bad}: {err}");
    }
    // The output side is guarded the same way.
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let err = convert(
        &cfg,
        json!({"path": src.to_str().unwrap(), "to": "pdf", "output_path": "//attacker.invalid/s/a.pdf"}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
    assert!(!log.exists());
}

#[test]
fn rtf_input_is_not_supported() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let rtf = work.path().join("a.rtf");
    fs::write(&rtf, br"{\rtf1 hi}").unwrap();
    let err = convert(&cfg, json!({"path": rtf.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("cannot convert"), "{err}");
    let err = run_preview(&cfg, json!({"path": rtf.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("cannot preview"), "{err}");
    assert!(!log.exists());
}

#[test]
fn plain_only_vetting_runs_before_soffice_for_the_new_rules() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let log = tools.path().join("args.txt");
    let cfg = cfg_in(work.path(), Some(fake_soffice(tools.path(), &log)), None);
    let hyperlink_rel = "<Relationships><Relationship Id=\"r1\" Type=\"x/hyperlink\" Target=\"http://attacker.invalid/p.png\" TargetMode=\"External\"/></Relationships>";
    let evil: Vec<(&str, Vec<u8>)> = vec![
        (
            "hyperlink-rel.docx",
            zip_with(&[
                ("word/document.xml", "<w:document/>"),
                ("word/_rels/document.xml.rels", hyperlink_rel),
            ]),
        ),
        (
            "cdata-field.docx",
            zip_with(&[(
                "word/document.xml",
                "<w:instrText><![CDATA[INCLUDEPICTURE \"x\"]]></w:instrText>",
            )]),
        ),
        (
            "webservice.xlsx",
            zip_with(&[(
                "xl/worksheets/sheet1.xml",
                "<worksheet><c><f>WEBSERVICE(\"http://127.0.0.1:1/\")</f></c></worksheet>",
            )]),
        ),
        (
            "blip-link.pptx",
            zip_with(&[("ppt/slides/slide1.xml", "<a:blip r:link=\"rId2\"/>")]),
        ),
        (
            "embedded.docx",
            zip_with(&[("word/embeddings/oleObject1.bin", "x")]),
        ),
        (
            "altchunk.docx",
            zip_with(&[("word/document.xml", "<w:altChunk r:id=\"rId5\"/>")]),
        ),
    ];
    for (name, bytes) in evil {
        let src = work.path().join(name);
        fs::write(&src, bytes).unwrap();
        let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
        assert!(
            err.to_string().contains("not plain enough"),
            "{name}: {err}"
        );
        assert!(!log.exists(), "{name}: soffice must not have been started");
    }
}

/// Floods the profile / TMP folder (not `out/`): the whole temp folder is capped too.
#[cfg(unix)]
#[test]
fn a_converter_that_fills_its_profile_folder_is_stopped() {
    let tools = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let flood = fake(
        tools.path(),
        "soffice",
        "#!/bin/sh\nyes x | head -c 400000 > \"$TMPDIR/flood.tmp\"\nsleep 30\n",
        "",
    );
    let mut cfg = cfg_in(work.path(), Some(flood), None);
    cfg.max_output_bytes = 1000;
    cfg.max_input_bytes = 1000;
    cfg.convert.timeout = Duration::from_secs(25);
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    let started = Instant::now();
    let err = convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(15));
}

/// A fake converter that tries to open a TCP connection to a listener on the host's
/// loopback: inside the network sandbox it must not get through. Skipped (visibly) where
/// no sandbox works, e.g. a CI runner that forbids user namespaces and has no bubblewrap.
#[cfg(target_os = "linux")]
#[test]
fn the_converter_cannot_open_a_socket_inside_the_sandbox() {
    use std::net::TcpListener;
    if crate::convert_sandbox::detect().is_none() || !Path::new("/bin/bash").exists() {
        eprintln!("SKIPPED: no network sandbox (bwrap / unshare) or no bash on this host");
        return;
    }
    // /var/tmp is not hidden by bubblewrap's private /tmp.
    let tools = tempfile::Builder::new().tempdir_in("/var/tmp").unwrap();
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let soffice = fake(
        tools.path(),
        "soffice",
        &format!(
            "#!/bin/bash\nfor a in \"$@\"; do case \"$prev\" in --outdir) out=\"$a\";; esac; prev=\"$a\"; done\nif (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then echo CONNECTED > \"$out/input.pdf\"; else echo ISOLATED > \"$out/input.pdf\"; fi\n"
        ),
        "",
    );
    let mut cfg = cfg_in(work.path(), Some(soffice), None);
    crate::convert_sandbox::force_no_sandbox(false);
    cfg.convert.allow_unsandboxed_convert = false;
    let src = work.path().join("a.docx");
    fs::write(&src, docx_bytes()).unwrap();
    convert(&cfg, json!({"path": src.to_str().unwrap(), "to": "pdf"})).unwrap();
    let made = fs::read_to_string(work.path().join("a.pdf")).unwrap();
    assert_eq!(made.trim(), "ISOLATED", "the converter reached the network");
    assert!(listener.accept().is_err(), "the listener saw a connection");
}
