//! Locates the `test_server` example without assuming Cargo's build-directory
//! layout (it moved from `target/debug/deps/<test>` to
//! `target/debug/build/<pkg>/<hash>/out/<test>` in newer toolchains).
//!
//! Asks Cargo itself: `cargo build --example test_server --message-format=json`
//! reports the executable path of the artifact it built (a no-op rebuild when
//! `cargo test` already produced it). Once per test binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Absolute path of the built `test_server` example executable.
pub fn test_server() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(build)
}

fn build() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--quiet", "--example", "test_server"])
        .args(["--message-format", "json-render-diagnostics"])
        .arg("--manifest-path")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    // Reuse the target directory of the running `cargo test` (the parent of
    // `<target>/tmp`) so an explicit `--target-dir` is honoured too.
    if let Some(tmp) = option_env!("CARGO_TARGET_TMPDIR")
        && let Some(target) = Path::new(tmp).parent()
    {
        command.arg("--target-dir").arg(target);
    }
    let output = command.output().expect("run `cargo build --example`");
    assert!(
        output.status.success(),
        "`cargo build --example test_server` failed"
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == "test_server")
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .expect("cargo did not report a test_server executable")
}
