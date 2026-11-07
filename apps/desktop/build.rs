use std::env;
use std::fs;
use std::path::Path;

fn main() {
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

    tauri_build::build();
}
