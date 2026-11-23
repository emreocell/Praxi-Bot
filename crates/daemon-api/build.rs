//! Records the git short hash of the build so two untagged builds of the same cargo
//! version can be told apart (`GET /v1/host` -> `build_id`, `bot --version`).
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PRAXI_RELEASE_TAG");
    println!("cargo:rerun-if-env-changed=PRAXI_BUILD_HASH");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    let hash = std::env::var("PRAXI_BUILD_HASH")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| git(&["rev-parse", "--short=8", "HEAD"]))
        .filter(|h| h.chars().all(|c| c.is_ascii_alphanumeric()));
    if let Some(hash) = hash {
        println!("cargo:rustc-env=PRAXI_BUILD_HASH={hash}");
    }
}
