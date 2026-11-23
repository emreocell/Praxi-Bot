#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--stop-for-uninstall")) {
        let result = std::env::current_exe().and_then(|exe| {
            // Updates use a fresh helper extracted by NSIS; older installed
            // desktops may not recognise this helper flag.
            let directory = std::env::args_os()
                .nth(2)
                .map_or_else(
                    || exe.parent().map(std::path::Path::to_path_buf),
                    |path| Some(std::path::PathBuf::from(path)),
                )
                .ok_or_else(|| std::io::Error::other("Praxi installation folder is unavailable"))?;
            if !directory.is_absolute() {
                return Err(std::io::Error::other(
                    "Praxi installation folder must be an absolute path",
                ));
            }
            process_guard::supervision::stop_installation(&directory)?;
            // A daemon reused from another installation is deliberately not killed.
            // Refuse uninstall/data removal while that data directory remains owned.
            if let Some(data) = storage::data_dir() {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(data.join(process_guard::lifecycle::LOCK_FILE))
                {
                    Ok(file) => file.try_lock().map_err(std::io::Error::other)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    desktop_lib::run();
}
