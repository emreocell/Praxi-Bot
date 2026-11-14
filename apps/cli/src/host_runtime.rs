//! Host desktop selection, shared with `bot-node`.
use std::sync::Arc;

use desktop_api::DesktopBackend;

pub fn local_desktop(warn: &mut dyn FnMut(&str)) -> Option<Arc<dyn DesktopBackend>> {
    if desktop_off(std::env::var("BOT_DESKTOP").ok().as_deref()) {
        return None;
    }
    match backend() {
        Ok(backend) => Some(backend),
        Err(error) => {
            warn(&format!("desktop tools disabled: {error}"));
            None
        }
    }
}

fn desktop_off(value: Option<&str>) -> bool {
    matches!(value, Some("off" | "0" | "false"))
}

#[cfg(any(target_os = "linux", test))]
fn display_session(
    display: Option<&str>,
    wayland: Option<&str>,
    session: Option<&str>,
) -> Result<String, String> {
    desktop_linux::pure::detect_display_session(session, wayland, display)
        .map_err(|e| e.to_string())
}

pub fn backend() -> Result<Arc<dyn DesktopBackend>, String> {
    #[cfg(windows)]
    {
        desktop_windows::WindowsBackend::new()
            .map(|b| Arc::new(b) as Arc<dyn DesktopBackend>)
            .map_err(|e| format!("cannot start the Windows backend: {e}"))
    }
    #[cfg(target_os = "linux")]
    {
        let display = std::env::var("DISPLAY").ok();
        let session = std::env::var("XDG_SESSION_TYPE").ok();
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        display_session(display.as_deref(), wayland.as_deref(), session.as_deref())?;
        let backend = desktop_linux::LinuxBackend::new().map_err(|e| e.to_string())?;
        if !backend.capabilities().has_display {
            return Err("X11 DISPLAY is unreachable; check Xvfb, DISPLAY and XAUTHORITY".into());
        }
        Ok(Arc::new(backend))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    Err(format!(
        "desktop control is not supported on {} yet",
        std::env::consts::OS
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_selection_is_pure_and_desktop_off_wins() {
        for value in [Some("off"), Some("0"), Some("false")] {
            assert!(desktop_off(value));
        }
        assert!(!desktop_off(None));
        assert!(!desktop_off(Some("on")));
        assert!(display_session(None, None, None).is_err());
        assert!(display_session(Some("  "), None, Some("x11")).is_err());
        assert!(display_session(None, Some("wayland-0"), Some("wayland")).is_err());
        assert_eq!(display_session(Some(":99"), None, None).unwrap(), ":99");
        // An explicit X11 session (the node --display child) takes precedence.
        assert_eq!(
            display_session(Some(":99"), Some("wayland-0"), Some("x11")).unwrap(),
            ":99"
        );
    }
}
