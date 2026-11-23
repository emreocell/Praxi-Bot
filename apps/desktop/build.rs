use std::env;
use std::fs;
use std::path::Path;

include!("src/command_names.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/command_names.rs");
    let dist_dir = Path::new("ui/dist");
    if !dist_dir.exists() {
        let _ = fs::create_dir_all(dist_dir);
    }

    if let Ok(target) = env::var("TARGET") {
        println!("cargo:rustc-env=TAURI_TARGET_TRIPLE={target}");
        let ext = if target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        let bin_dir = Path::new("binaries");
        if !bin_dir.exists() {
            let _ = fs::create_dir_all(bin_dir);
        }
        let sidecar = bin_dir.join(format!("bot-{target}{ext}"));
        let sidecar_non_empty = fs::metadata(&sidecar).is_ok_and(|m| m.is_file() && m.len() > 0);
        if !sidecar_non_empty {
            let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_owned());
            let built_bot = Path::new("..")
                .join("..")
                .join("target")
                .join(&profile)
                .join(format!("bot{ext}"));
            let copied = if fs::metadata(&built_bot).is_ok_and(|m| m.is_file() && m.len() > 0) {
                fs::copy(&built_bot, &sidecar).is_ok()
            } else {
                false
            };
            if !copied && !sidecar.exists() {
                let _ = fs::write(&sidecar, []);
            }
        }
    }

    // Integration tests link the Tauri runtime; on Windows their exe needs the
    // common-controls v6 manifest to start (bins get it from tauri-build).
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && let Ok(dir) = env::var("CARGO_MANIFEST_DIR")
    {
        println!("cargo:rerun-if-changed=windows-test.manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{dir}/windows-test.manifest");
    }

    // Every custom command needs an explicit `allow-<command>` permission in a
    // capability; without the app manifest they would be callable from every
    // window.
    if let Err(error) = tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMAND_NAMES)),
    ) {
        println!("{error:#}");
        std::process::exit(1);
    }
}
