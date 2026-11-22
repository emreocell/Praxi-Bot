//! Keeps realistic-looking fake keys out of the source tree (see
//! docs/contributing/secret-scanning.md). Fixtures must be built at runtime.

use std::path::{Path, PathBuf};

use regex::Regex;

/// Files allowed to contain such literals (generated gitleaks table).
const ALLOWED: &[&str] = &["crates/secret-store/src/rules.rs"];

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", "build"];
const EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "json", "md", "toml", "yml", "yaml", "py", "sh", "txt",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&p, out);
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| EXTS.contains(&x))
        {
            out.push(p);
        }
    }
}

#[test]
fn no_realistic_google_key_or_telegram_token_literals_outside_allow_list() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Built from pieces so this file does not match itself.
    let google = format!("AIza{}", "[0-9A-Za-z_-]{35}");
    let telegram = format!("[0-9]{{8,10}}{}[A-Za-z0-9_-]{{35}}", ":");
    let re = Regex::new(&format!("{google}|{telegram}")).unwrap();

    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        files.len() > 100,
        "walk found too few files: {}",
        files.len()
    );

    let mut hits = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if ALLOWED.contains(&rel.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "realistic key/token literals found (build them at runtime, see docs/contributing/secret-scanning.md): {hits:?}"
    );
}
