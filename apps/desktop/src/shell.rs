//! Tray icon and native notifications of the desktop shell.
//!
//! The UI owns *what* to notify about (it knows the runs, the settings and the
//! window focus); this module owns *how*: the toast (with click-to-open), the
//! tray menu and the "needs you" count shown in the tray tooltip and menu.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, Wry};

use crate::control::ControlHandle;
use crate::daemon::DaemonHandle;

/// A notification was clicked (payload: [`ActivatedPayload`]).
pub const EVENT_NOTIFICATION_ACTIVATED: &str = "shell:notification_activated";
/// Tray menu "Needs you" was chosen.
pub const EVENT_TRAY_OPEN_NEEDS: &str = "shell:tray_open_needs";
/// Tray menu "Stop all" was chosen (the UI cancels the daemon's runs).
pub const EVENT_TRAY_STOP_ALL: &str = "shell:tray_stop_all";

const TRAY_ID: &str = "main";
const MENU_TOGGLE: &str = "tray.toggle";
const MENU_NEEDS: &str = "tray.needs";
const MENU_STOP_ALL: &str = "tray.stop_all";
const MENU_QUIT: &str = "tray.quit";

/// Payload of [`EVENT_NOTIFICATION_ACTIVATED`].
#[derive(Debug, Clone, Serialize)]
pub struct ActivatedPayload {
    /// The chat the notification was about, if any.
    pub chat_id: Option<String>,
}

/// Tray strings; `{n}` in the `needs*` templates is the count.  Sent by the UI
/// so they follow its language.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    /// "Show / hide window".
    pub toggle: String,
    /// "Needs you ({n})".
    pub needs: String,
    /// "Stop all".
    pub stop_all: String,
    /// "Quit".
    pub quit: String,
    /// Tooltip with nothing pending.
    pub tooltip: String,
    /// Tooltip with `{n}` things pending.
    pub tooltip_needs: String,
}

impl Default for TrayLabels {
    fn default() -> Self {
        Self {
            toggle: "Show / hide window".to_owned(),
            needs: "Needs you ({n})".to_owned(),
            stop_all: "Stop all".to_owned(),
            quit: "Quit".to_owned(),
            tooltip: "Praxi-Bot".to_owned(),
            tooltip_needs: "Praxi-Bot: {n} need you".to_owned(),
        }
    }
}

/// Fill the `{n}` placeholder of a template.
#[must_use]
pub fn with_count(template: &str, n: u32) -> String {
    template.replace("{n}", &n.to_string())
}

/// The tray tooltip for `n` pending items.
#[must_use]
pub fn tooltip_for(labels: &TrayLabels, n: u32) -> String {
    if n == 0 {
        labels.tooltip.clone()
    } else {
        with_count(&labels.tooltip_needs, n)
    }
}

struct TrayParts {
    tray: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
    needs: MenuItem<Wry>,
    stop_all: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Managed state of the tray.
#[derive(Default)]
pub struct TrayState {
    labels: Mutex<TrayLabels>,
    needs: Mutex<u32>,
    parts: Mutex<Option<TrayParts>>,
}

impl TrayState {
    fn refresh(&self) {
        let labels = self.labels.lock().clone();
        let n = *self.needs.lock();
        if let Some(p) = self.parts.lock().as_ref() {
            let _ = p.toggle.set_text(&labels.toggle);
            let _ = p.needs.set_text(with_count(&labels.needs, n));
            let _ = p.needs.set_enabled(n > 0);
            let _ = p.stop_all.set_text(&labels.stop_all);
            let _ = p.quit.set_text(&labels.quit);
            let _ = p.tray.set_tooltip(Some(tooltip_for(&labels, n)));
        }
    }
}

/// Bring the main window to the front.
pub fn focus_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Show the main window, or hide it when it is already in front.
fn toggle_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let showing = w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false);
        if showing {
            let _ = w.hide();
        } else {
            focus_main(app);
        }
    }
}

/// Stop the daemon if this app started it, then exit.
fn quit(app: &AppHandle) {
    if let Some(daemon) = app.try_state::<Arc<DaemonHandle>>() {
        daemon.stop();
    }
    app.exit(0);
}

/// Create the tray icon and its menu.
///
/// # Errors
/// If a menu item or the tray icon cannot be created.
pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<TrayState>();
    let labels = state.labels.lock().clone();
    let toggle = MenuItem::with_id(app, MENU_TOGGLE, &labels.toggle, true, None::<&str>)?;
    let needs = MenuItem::with_id(
        app,
        MENU_NEEDS,
        with_count(&labels.needs, 0),
        false,
        None::<&str>,
    )?;
    let stop_all = MenuItem::with_id(app, MENU_STOP_ALL, &labels.stop_all, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, MENU_QUIT, &labels.quit, true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &needs, &stop_all, &sep, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(tooltip_for(&labels, 0))
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_TOGGLE => toggle_main(app),
            MENU_NEEDS => {
                focus_main(app);
                let _ = app.emit(EVENT_TRAY_OPEN_NEEDS, ());
            }
            MENU_STOP_ALL => {
                if let Some(control) = app.try_state::<ControlHandle>() {
                    let _ = control.stop_all();
                }
                let _ = app.emit(EVENT_TRAY_STOP_ALL, ());
            }
            MENU_QUIT => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    *state.parts.lock() = Some(TrayParts {
        tray,
        toggle,
        needs,
        stop_all,
        quit: quit_item,
    });
    Ok(())
}

/// UI -> tray: how many approvals / questions wait for the user.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri commands take owned arguments
pub fn set_needs_count(state: State<'_, TrayState>, count: u32) {
    *state.needs.lock() = count;
    state.refresh();
}

/// UI -> tray: menu / tooltip strings in the UI language.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri commands take owned arguments
pub fn set_tray_labels(state: State<'_, TrayState>, labels: TrayLabels) {
    *state.labels.lock() = labels;
    state.refresh();
}

/// Show a native notification; clicking it brings the app to the front and
/// emits [`EVENT_NOTIFICATION_ACTIVATED`] so the UI can open the chat.
///
/// # Errors
/// If the OS refuses the notification.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri commands take owned arguments
pub fn notify_show(
    app: AppHandle,
    title: String,
    body: String,
    chat_id: Option<String>,
) -> Result<(), String> {
    show_native(&app, &title, &body, chat_id)
}

/// The desktop notification plugin cannot report clicks, so on Windows the
/// toast is built directly (`tauri-winrt-notification` is what the plugin uses
/// underneath) to get the activation callback.
#[cfg(windows)]
fn show_native(
    app: &AppHandle,
    title: &str,
    body: &str,
    chat_id: Option<String>,
) -> Result<(), String> {
    use std::path::Path;

    use tauri_winrt_notification::Toast;

    // An unpackaged dev build has no registered app id: borrow PowerShell's.
    let dev = tauri::utils::platform::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .is_some_and(|dir| {
            dir.ends_with(Path::new("target").join("debug"))
                || dir.ends_with(Path::new("target").join("release"))
        });
    let app_id = if dev {
        Toast::POWERSHELL_APP_ID.to_owned()
    } else {
        app.config().identifier.clone()
    };
    let handle = app.clone();
    Toast::new(&app_id)
        .title(title)
        .text1(body)
        .on_activated(move |_| {
            focus_main(&handle);
            let _ = handle.emit(
                EVENT_NOTIFICATION_ACTIVATED,
                ActivatedPayload {
                    chat_id: chat_id.clone(),
                },
            );
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn show_native(
    app: &AppHandle,
    title: &str,
    body: &str,
    _chat_id: Option<String>,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;

    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{TrayLabels, tooltip_for, with_count};

    #[test]
    fn count_fills_the_placeholder() {
        assert_eq!(with_count("Needs you ({n})", 3), "Needs you (3)");
        assert_eq!(with_count("none", 3), "none");
    }

    #[test]
    fn tooltip_shows_the_count_only_when_something_waits() {
        let l = TrayLabels::default();
        assert_eq!(tooltip_for(&l, 0), "Praxi-Bot");
        assert_eq!(tooltip_for(&l, 2), "Praxi-Bot: 2 need you");
    }

    #[test]
    fn labels_parse_from_the_ui_payload() {
        let l: TrayLabels = serde_json::from_str(
            r#"{"toggle":"a","needs":"b {n}","stopAll":"c","quit":"d","tooltip":"e","tooltipNeeds":"f {n}"}"#,
        )
        .expect("labels");
        assert_eq!(l.stop_all, "c");
        assert_eq!(tooltip_for(&l, 1), "f 1");
    }
}
