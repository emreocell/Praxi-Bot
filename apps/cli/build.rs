//! Windows: embed the Praxi ghost icon and version info in `bot.exe`, the
//! daemon sidecar, so Explorer, Task Manager and "Open file location" show the
//! same icon as the desktop app instead of the generic executable icon.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../desktop/icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut resource = tauri_winres::WindowsResource::new();
            resource.set_icon("../desktop/icons/icon.ico");
            resource.set("ProductName", "Praxi-Bot");
            resource.set("FileDescription", "Praxi-Bot daemon");
            if let Err(error) = resource.compile() {
                println!("cargo:warning=could not embed the Windows icon in bot.exe: {error}");
            }
        }
    }
}
