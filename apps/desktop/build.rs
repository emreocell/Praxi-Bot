use std::fs;
use std::path::Path;

fn main() {
    let dist_dir = Path::new("ui/dist");
    if !dist_dir.exists() {
        let _ = fs::create_dir_all(dist_dir);
    }
    tauri_build::build();
}
