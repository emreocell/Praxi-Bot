//! Tests for `doc_convert` / `doc_preview` with fake `soffice` / `pdftoppm` scripts.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tool_registry::ToolError;

use crate::DocToolsConfig;
use crate::common::Reply;
use crate::convert::{ConvertConfig, run_convert, run_preview};

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
    DocToolsConfig {
        convert: ConvertConfig {
            soffice_path: Some(soffice.unwrap_or_else(|| PathBuf::from("/nonexistent/soffice"))),
            pdftoppm_path: Some(ppm.unwrap_or_else(|| PathBuf::from("/nonexistent/pdftoppm"))),
            timeout: Duration::from_secs(20),
            ..ConvertConfig::default()
        },
        ..DocToolsConfig::default()
    }
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
    let cfg = cfg_with(Some(fake_soffice(tools.path(), &log)), None);
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
    let cfg = cfg_with(Some(fake_soffice(tools.path(), &log)), None);
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
    let cfg = cfg_with(None, None);
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
    let mut cfg = cfg_with(Some(hang), None);
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
    let mut cfg = cfg_with(Some(fake_soffice(tools.path(), &log)), None);
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
    let cfg = cfg_with(None, Some(fake_pdftoppm(tools.path())));
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
