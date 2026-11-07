//! Desktop shell library entry point for Tauri.

pub mod control;
pub mod daemon;

use std::sync::Arc;
use std::time::Duration;

use control::{
    BotRunMeta, ControlHandle, ControlMode, ControlSnapshot, DEFAULT_PRE_ACTION_HIGHLIGHT_MS,
    MonitorGeometry, OverlayClickPayload, OverlayEventSink, OverlayRect, OverlayShowTargetPayload,
    OverlayWireEvent, WIRE_EVENT_CONTROL_STATE, WIRE_EVENT_OVERLAY_CLEAR, WIRE_EVENT_OVERLAY_CLICK,
    WIRE_EVENT_OVERLAY_ENVELOPE, WIRE_EVENT_OVERLAY_SHOW_TARGET, spawn_global_hotkey_listener,
};
use daemon::{CliOutput, Connection, DaemonHandle};
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

const MENU_ID_SIMULATE_RUN: &str = "dev.simulate_bot_run";
const MENU_ID_TOGGLE_WHEEL: &str = "dev.toggle_wheel";
const MENU_ID_STOP_ALL: &str = "dev.stop_all";

const WIDGET_WIDTH_LOGICAL: f64 = 360.0;
const WIDGET_HEIGHT_LOGICAL: f64 = 188.0;
const WIDGET_MARGIN_RIGHT_LOGICAL: f64 = 28.0;
const WIDGET_MARGIN_BOTTOM_LOGICAL: f64 = 72.0;

struct TauriOverlaySink {
    app: AppHandle,
}

impl OverlayEventSink for TauriOverlaySink {
    fn on_show_target(&self, payload: &OverlayShowTargetPayload) {
        let _ = self.app.emit(WIRE_EVENT_OVERLAY_SHOW_TARGET, payload);
        let _ = self.app.emit(
            WIRE_EVENT_OVERLAY_ENVELOPE,
            OverlayWireEvent::ShowTarget(payload.clone()),
        );
    }

    fn on_click(&self, payload: &OverlayClickPayload) {
        let _ = self.app.emit(WIRE_EVENT_OVERLAY_CLICK, payload);
        let _ = self.app.emit(
            WIRE_EVENT_OVERLAY_ENVELOPE,
            OverlayWireEvent::Click(*payload),
        );
    }

    fn on_clear(&self) {
        let _ = self
            .app
            .emit(WIRE_EVENT_OVERLAY_CLEAR, serde_json::json!({}));
        let _ = self
            .app
            .emit(WIRE_EVENT_OVERLAY_ENVELOPE, OverlayWireEvent::Clear);
    }

    fn on_state_changed(&self, snapshot: &ControlSnapshot) {
        sync_auxiliary_windows(&self.app, snapshot.mode);
        let _ = self.app.emit(WIRE_EVENT_CONTROL_STATE, snapshot);
    }
}

fn collect_monitors_geometry(app: &AppHandle) -> Vec<MonitorGeometry> {
    let Ok(monitors) = app.available_monitors() else {
        return vec![default_monitor_geometry()];
    };
    if monitors.is_empty() {
        return vec![default_monitor_geometry()];
    }
    monitors
        .into_iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let pos = monitor.position();
            let size = monitor.size();
            MonitorGeometry {
                monitor_index: idx,
                physical_x: f64::from(pos.x),
                physical_y: f64::from(pos.y),
                physical_width: f64::from(size.width),
                physical_height: f64::from(size.height),
                scale_factor: monitor.scale_factor(),
            }
        })
        .collect()
}

fn default_monitor_geometry() -> MonitorGeometry {
    MonitorGeometry {
        monitor_index: 0,
        physical_x: 0.0,
        physical_y: 0.0,
        physical_width: 1920.0,
        physical_height: 1080.0,
        scale_factor: 1.0,
    }
}

fn ensure_auxiliary_windows(app: &AppHandle) {
    let monitors = collect_monitors_geometry(app);

    for mon in &monitors {
        let label = format!("overlay-{}", mon.monitor_index);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let phys_x = mon.physical_x.round() as i32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let phys_y = mon.physical_y.round() as i32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let phys_w = mon.physical_width.round().max(100.0) as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let raw_phys_h = mon.physical_height.round().max(100.0) as u32;
        // Subtract 1 physical pixel on Windows so DWM never switches a borderless popup
        // into exclusive full-screen composition (which would disable transparency).
        let phys_h = if cfg!(windows) {
            raw_phys_h.saturating_sub(1).max(1)
        } else {
            raw_phys_h
        };

        if let Some(existing) = app.get_webview_window(&label) {
            let _ = existing.set_position(PhysicalPosition::new(phys_x, phys_y));
            let _ = existing.set_size(PhysicalSize::new(phys_w, phys_h));
            let _ = existing.set_ignore_cursor_events(true);
            continue;
        }

        let query_url = format!(
            "index.html?window=overlay&monitorIndex={}&physX={}&physY={}&physW={}&physH={}&scale={}",
            mon.monitor_index,
            mon.physical_x,
            mon.physical_y,
            mon.physical_width,
            mon.physical_height,
            mon.scale_factor
        );

        let builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(query_url.into()))
            .title("Praxi-Bot Overlay")
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .shadow(false)
            .resizable(false)
            .focused(false)
            .focusable(false)
            .visible(false);

        if let Ok(window) = builder.build() {
            let _ = window.set_position(PhysicalPosition::new(phys_x, phys_y));
            let _ = window.set_size(PhysicalSize::new(phys_w, phys_h));
            let _ = window.set_ignore_cursor_events(true);
        }
    }

    if app.get_webview_window("widget").is_none() {
        let primary = monitors
            .first()
            .copied()
            .unwrap_or_else(default_monitor_geometry);
        let scale = primary.effective_scale();
        let logical_screen_w = primary.physical_width / scale;
        let logical_screen_h = primary.physical_height / scale;
        let pos_x =
            (logical_screen_w - WIDGET_WIDTH_LOGICAL - WIDGET_MARGIN_RIGHT_LOGICAL).max(16.0);
        let pos_y =
            (logical_screen_h - WIDGET_HEIGHT_LOGICAL - WIDGET_MARGIN_BOTTOM_LOGICAL).max(16.0);

        let builder = WebviewWindowBuilder::new(
            app,
            "widget",
            WebviewUrl::App("index.html?window=widget".into()),
        )
        .title("Praxi-Bot Mini Widget")
        .inner_size(WIDGET_WIDTH_LOGICAL, WIDGET_HEIGHT_LOGICAL)
        .position(pos_x, pos_y)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .resizable(false)
        .focused(false)
        .visible(false);

        if let Ok(widget_win) = builder.build() {
            let _ = widget_win.set_size(LogicalSize::new(
                WIDGET_WIDTH_LOGICAL,
                WIDGET_HEIGHT_LOGICAL,
            ));
            let _ = widget_win.set_position(LogicalPosition::new(pos_x, pos_y));
        }
    }
}

fn sync_auxiliary_windows(app: &AppHandle, mode: ControlMode) {
    let app_clone = app.clone();
    let _ = app.run_on_main_thread(move || {
        ensure_auxiliary_windows(&app_clone);
        let monitors = collect_monitors_geometry(&app_clone);

        if mode == ControlMode::Idle {
            if let Some(widget) = app_clone.get_webview_window("widget") {
                let _ = widget.hide();
            }
            for mon in &monitors {
                let label = format!("overlay-{}", mon.monitor_index);
                if let Some(overlay) = app_clone.get_webview_window(&label) {
                    let _ = overlay.hide();
                }
            }
        } else {
            for mon in &monitors {
                let label = format!("overlay-{}", mon.monitor_index);
                if let Some(overlay) = app_clone.get_webview_window(&label) {
                    let _ = overlay.set_ignore_cursor_events(true);
                    let _ = overlay.show();
                }
            }
            if let Some(widget) = app_clone.get_webview_window("widget") {
                let _ = widget.show();
            }
        }
    });
}

// ── Dev Mock Scripted Simulation ──────────────────────────────────────────────

struct SimulationStep {
    step_text: &'static str,
    target_label: &'static str,
    action: &'static str,
    typed_text: Option<&'static str>,
    rel_x: f64,
    rel_y: f64,
    rel_w: f64,
    rel_h: f64,
    hold_ms: u64,
}

const SIMULATION_STEPS: &[SimulationStep] = &[
    SimulationStep {
        step_text: "Focusing workspace search bar",
        target_label: "Search bar",
        action: "click",
        typed_text: None,
        rel_x: 0.22,
        rel_y: 0.18,
        rel_w: 0.24,
        rel_h: 0.055,
        hold_ms: 1100,
    },
    SimulationStep {
        step_text: "Typing query into search bar",
        target_label: "Search bar",
        action: "type",
        typed_text: Some("quarterly revenue report 2026"),
        rel_x: 0.22,
        rel_y: 0.18,
        rel_w: 0.28,
        rel_h: 0.055,
        hold_ms: 1500,
    },
    SimulationStep {
        step_text: "Selecting Q4 verified filter",
        target_label: "Filter: Q4 Verified",
        action: "click",
        typed_text: None,
        rel_x: 0.58,
        rel_y: 0.28,
        rel_w: 0.16,
        rel_h: 0.05,
        hold_ms: 1100,
    },
    SimulationStep {
        step_text: "Entering review note",
        target_label: "Review note input",
        action: "type",
        typed_text: Some("Verified by Praxi-Bot — ready for sign-off"),
        rel_x: 0.30,
        rel_y: 0.52,
        rel_w: 0.34,
        rel_h: 0.07,
        hold_ms: 1500,
    },
    SimulationStep {
        step_text: "Clicking Submit approval button",
        target_label: "Submit Approval",
        action: "click",
        typed_text: None,
        rel_x: 0.44,
        rel_y: 0.72,
        rel_w: 0.14,
        rel_h: 0.055,
        hold_ms: 1200,
    },
];

fn spawn_simulated_bot_run(
    app: &AppHandle,
    control: ControlHandle,
    bot_override: Option<BotRunMeta>,
) {
    let bot = bot_override.unwrap_or_else(|| BotRunMeta {
        bot_id: "atlas".to_string(),
        bot_name: "Atlas".to_string(),
        bot_avatar: None,
        bot_color: Some("#3b82f6".to_string()),
        bot_initials: Some("AT".to_string()),
    });

    let initial = control.start_run(bot, "Preparing desktop automation...");
    let expected_gen = initial.run_generation;
    let primary = collect_monitors_geometry(app)
        .into_iter()
        .next()
        .unwrap_or_else(default_monitor_geometry);

    tauri::async_runtime::spawn(async move {
        if cfg!(debug_assertions) {
            eprintln!(
                "[simulate_bot_run] started run_generation={expected_gen} on monitor={} ({}x{} @ scale={})",
                primary.monitor_index,
                primary.physical_width,
                primary.physical_height,
                primary.scale_factor
            );
        }

        // Give the overlay & mini widget windows a moment to render on screen.
        tokio::time::sleep(Duration::from_millis(350)).await;

        for (idx, step) in SIMULATION_STEPS.iter().enumerate() {
            let rect = OverlayRect {
                x: primary.physical_x + (primary.physical_width * step.rel_x).round(),
                y: primary.physical_y + (primary.physical_height * step.rel_y).round(),
                width: (primary.physical_width * step.rel_w).round().max(120.0),
                height: (primary.physical_height * step.rel_h).round().max(40.0),
            };

            // Wait loop in case the user paused the run or took the wheel.
            loop {
                let snap = control.snapshot();
                if snap.mode == ControlMode::Idle || snap.run_generation != expected_gen {
                    return;
                }

                control.set_current_step(step.step_text);

                // Attempt to begin the action (~300 ms highlight before action).
                // When in HumanDriving or Paused, `begin_action_with_detail` refuses (`false`).
                let proceed = control
                    .begin_action_with_detail(
                        rect,
                        step.target_label,
                        step.action,
                        step.typed_text.map(str::to_string),
                    )
                    .await;

                if proceed {
                    if cfg!(debug_assertions) {
                        eprintln!(
                            "[simulate_bot_run] step {}/{}: action={} label={:?} rect=({}, {}, {}x{}) proceed=true",
                            idx + 1,
                            SIMULATION_STEPS.len(),
                            step.action,
                            step.target_label,
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height
                        );
                    }
                    break;
                }

                // Wait while paused or while the human holds the wheel before retrying.
                tokio::time::sleep(Duration::from_millis(250)).await;
            }

            let snap = control.snapshot();
            if snap.mode == ControlMode::Idle || snap.run_generation != expected_gen {
                return;
            }

            if step.action == "click" {
                let (cx, cy) = rect.center();
                control.click(cx, cy);
                if cfg!(debug_assertions) {
                    eprintln!("[simulate_bot_run] overlay.click at ({cx}, {cy})");
                }
            }

            // Hold so the user can observe the highlight / ripple / typed-text preview,
            // checking periodically for Stop.
            let mut remaining = step.hold_ms;
            while remaining > 0 {
                let slice = remaining.min(100);
                tokio::time::sleep(Duration::from_millis(slice)).await;
                remaining = remaining.saturating_sub(slice);

                let current = control.snapshot();
                if current.mode == ControlMode::Idle || current.run_generation != expected_gen {
                    return;
                }
            }
        }

        let final_snap = control.snapshot();
        if final_snap.run_generation == expected_gen && final_snap.mode != ControlMode::Idle {
            control.stop_all();
            if cfg!(debug_assertions) {
                eprintln!(
                    "[simulate_bot_run] completed all steps; overlay.clear emitted and state returned to Idle"
                );
            }
        }
    });
}

// ── Tauri Commands ────────────────────────────────────────────────────────────

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn get_control_state(control: State<'_, ControlHandle>) -> ControlSnapshot {
    control.snapshot()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn is_human_driving(control: State<'_, ControlHandle>) -> bool {
    control.is_human_driving()
}

#[tauri::command]
async fn begin_action(
    control: State<'_, ControlHandle>,
    rect: Option<OverlayRect>,
    label: String,
    action: Option<String>,
    text: Option<String>,
) -> Result<bool, String> {
    let action_kind = action.unwrap_or_else(|| "click".to_string());
    Ok(match rect {
        Some(rect) => {
            control
                .begin_action_with_detail(rect, label, action_kind, text)
                .await
        }
        // No target (key press, launch): no highlight, only the step text.
        None => control.begin_action_untargeted(label).await,
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn start_bot_run(
    control: State<'_, ControlHandle>,
    bot: BotRunMeta,
    initial_step: String,
) -> ControlSnapshot {
    control.start_run(bot, initial_step)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn pause_bot_run(control: State<'_, ControlHandle>) -> Result<ControlSnapshot, String> {
    control.pause().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn resume_bot_run(control: State<'_, ControlHandle>) -> Result<ControlSnapshot, String> {
    control.resume().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn take_the_wheel(control: State<'_, ControlHandle>) -> Result<ControlSnapshot, String> {
    control.take_the_wheel().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn give_back_control(control: State<'_, ControlHandle>) -> Result<ControlSnapshot, String> {
    control.give_back().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn stop_bot_run(control: State<'_, ControlHandle>) -> ControlSnapshot {
    control.stop_all()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn set_stop_hotkey(
    control: State<'_, ControlHandle>,
    hotkey: String,
) -> Result<ControlSnapshot, String> {
    control.set_stop_hotkey(hotkey).map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_show_target(
    control: State<'_, ControlHandle>,
    rect: OverlayRect,
    label: String,
    action: String,
    text: Option<String>,
) {
    control.show_target(rect, label, action, text);
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_click(control: State<'_, ControlHandle>, x: f64, y: f64) {
    control.click(x, y);
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_clear(control: State<'_, ControlHandle>) {
    control.clear();
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn simulate_bot_run(
    app: AppHandle,
    control: State<'_, ControlHandle>,
    bot: Option<BotRunMeta>,
) -> ControlSnapshot {
    spawn_simulated_bot_run(&app, control.inner().clone(), bot);
    control.snapshot()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn get_monitors_geometry(app: AppHandle) -> Vec<MonitorGeometry> {
    collect_monitors_geometry(&app)
}

// ── Daemon lifecycle / provider management (task 017) ─────────────────────────

/// Start the daemon if needed and return the base URL + bearer token.
#[tauri::command]
async fn daemon_connect(daemon: State<'_, Arc<DaemonHandle>>) -> Result<Connection, String> {
    daemon::connect(&daemon).await
}

/// Stop a daemon this app started, then start it again (e.g. to pick up new
/// providers).  A daemon started by someone else is left alone.
#[tauri::command]
async fn daemon_restart(daemon: State<'_, Arc<DaemonHandle>>) -> Result<Connection, String> {
    daemon.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    daemon::connect(&daemon).await
}

/// Arguments of `providers_cli` (typed so no free-form flag reaches the CLI).
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProvidersAction {
    List,
    Add,
    Test,
    Default,
}

fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `bot providers list|add|test|default`; the API key (if any) travels in the
/// child's environment, never on the command line.
#[tauri::command]
async fn providers_cli(
    action: ProvidersAction,
    id: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
) -> Result<CliOutput, String> {
    let mut args: Vec<String> = Vec::new();
    let mut key = None;
    if let ProvidersAction::List = action {
        args.push("list".into());
    } else {
        let id = id
            .filter(|i| valid_provider_id(i))
            .ok_or("invalid provider id")?;
        match action {
            ProvidersAction::Add => {
                args.extend(["add".into(), id]);
                if let Some(u) = base_url.filter(|u| !u.trim().is_empty()) {
                    if !(u.starts_with("http://") || u.starts_with("https://")) {
                        return Err("base URL must start with http:// or https://".into());
                    }
                    args.extend(["--base-url".into(), u]);
                }
                if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                    args.extend(["--model".into(), m]);
                }
                if api_key.as_deref().is_some_and(|k| !k.is_empty()) {
                    args.extend(["--key-env".into(), daemon::KEY_ENV.into()]);
                    key = api_key;
                }
            }
            ProvidersAction::Test => args.extend(["test".into(), id]),
            ProvidersAction::Default => {
                args.extend(["default".into(), id]);
                if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                    args.extend(["--model".into(), m]);
                }
            }
            ProvidersAction::List => {}
        }
    }
    daemon::run_providers_cli(args, key).await
}

const DEFAULT_AUTOSTART_REG_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_VALUE_NAME: &str = "Praxi-Bot";

fn autostart_reg_key() -> String {
    std::env::var("PRAXIBOT_AUTOSTART_REG_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_AUTOSTART_REG_KEY.to_owned())
}

#[cfg(windows)]
fn query_windows_autostart(reg_key: &str) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg")
        .args(["query", reg_key, "/v", AUTOSTART_VALUE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn write_windows_autostart(reg_key: &str, enabled: bool) -> Result<bool, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| format!("cannot locate executable: {e}"))?;
        let quoted = format!("\"{}\"", exe.display());
        let status = std::process::Command::new("reg")
            .args([
                "add",
                reg_key,
                "/v",
                AUTOSTART_VALUE_NAME,
                "/t",
                "REG_SZ",
                "/d",
                &quoted,
                "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("cannot update autostart registry entry: {e}"))?;
        if !status.success() {
            return Err("failed to enable Windows autostart".into());
        }
        Ok(true)
    } else {
        if query_windows_autostart(reg_key) {
            let _ = std::process::Command::new("reg")
                .args(["delete", reg_key, "/v", AUTOSTART_VALUE_NAME, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        Ok(false)
    }
}

/// Check whether per-user autostart (`HKCU\...\Run\Praxi-Bot`) is enabled.
/// Off by default.
#[tauri::command]
fn get_autostart() -> bool {
    #[cfg(windows)]
    {
        query_windows_autostart(&autostart_reg_key())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Enable or disable per-user Windows autostart (`HKCU\...\Run\Praxi-Bot`).
#[tauri::command]
fn set_autostart(enabled: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        write_windows_autostart(&autostart_reg_key(), enabled)
    }
    #[cfg(not(windows))]
    {
        Ok(enabled)
    }
}

fn setup_dev_menu(app: &tauri::App, control: ControlHandle) {
    if !cfg!(debug_assertions) {
        return;
    }

    if let Ok(simulate_item) = MenuItem::with_id(
        app,
        MENU_ID_SIMULATE_RUN,
        "Simulate bot run",
        true,
        None::<&str>,
    ) {
        let toggle_wheel_item = MenuItem::with_id(
            app,
            MENU_ID_TOGGLE_WHEEL,
            "Take the wheel / Give back",
            true,
            None::<&str>,
        )
        .ok();
        let stop_item = MenuItem::with_id(
            app,
            MENU_ID_STOP_ALL,
            "Stop all (Ctrl+Alt+Esc)",
            true,
            None::<&str>,
        )
        .ok();

        let mut items: Vec<&dyn tauri::menu::IsMenuItem<_>> = vec![&simulate_item];
        if let Some(ref wheel) = toggle_wheel_item {
            items.push(wheel);
        }
        if let Some(ref stop) = stop_item {
            items.push(stop);
        }

        if let Ok(dev_submenu) = Submenu::with_items(app, "Developer", true, &items)
            && let Ok(menu) = Menu::with_items(app, &[&dev_submenu])
        {
            let _ = app.set_menu(menu);
        }
    }

    app.on_menu_event(move |app_handle, event| match event.id().as_ref() {
        MENU_ID_SIMULATE_RUN => {
            spawn_simulated_bot_run(app_handle, control.clone(), None);
        }
        MENU_ID_TOGGLE_WHEEL => {
            let mode = control.snapshot().mode;
            if mode == ControlMode::HumanDriving {
                let _ = control.give_back();
            } else if mode != ControlMode::Idle {
                let _ = control.take_the_wheel();
            }
        }
        MENU_ID_STOP_ALL => {
            control.stop_all();
        }
        _ => {}
    });
}

/// Builds and runs the Tauri desktop application.
///
/// # Panics
///
/// Panics if the Tauri application fails to initialize or run.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let control = ControlHandle::new(Duration::from_millis(DEFAULT_PRE_ACTION_HIGHLIGHT_MS));
    let control_for_setup = control.clone();
    let daemon_handle = Arc::new(DaemonHandle::default());
    let daemon_for_exit = Arc::clone(&daemon_handle);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(control)
        .manage(daemon_handle)
        .setup(move |app| {
            let app_handle = app.handle().clone();
            control_for_setup.set_sink(Arc::new(TauriOverlaySink {
                app: app_handle.clone(),
            }));

            ensure_auxiliary_windows(&app_handle);
            spawn_global_hotkey_listener(control_for_setup.clone());
            setup_dev_menu(app, control_for_setup.clone());

            if cfg!(debug_assertions)
                && std::env::var("PRAXIBOT_AUTO_SIMULATE").as_deref() == Ok("1")
            {
                let app_for_sim = app_handle.clone();
                let control_for_sim = control_for_setup.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    spawn_simulated_bot_run(&app_for_sim, control_for_sim, None);
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                // `exit` may end the process without running `RunEvent::Exit`,
                // so stop a daemon this app started right here.
                if let Some(daemon) = window.app_handle().try_state::<Arc<DaemonHandle>>() {
                    daemon.stop();
                }
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_control_state,
            is_human_driving,
            begin_action,
            start_bot_run,
            pause_bot_run,
            resume_bot_run,
            take_the_wheel,
            give_back_control,
            stop_bot_run,
            set_stop_hotkey,
            overlay_show_target,
            overlay_click,
            overlay_clear,
            simulate_bot_run,
            get_monitors_geometry,
            daemon_connect,
            daemon_restart,
            providers_cli,
            get_autostart,
            set_autostart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |_app, event| {
        if matches!(event, RunEvent::Exit) {
            // Stop the daemon only if this app started it.
            daemon_for_exit.stop();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::valid_provider_id;

    #[test]
    fn provider_ids_cannot_smuggle_flags() {
        assert!(valid_provider_id("deepseek"));
        assert!(valid_provider_id("my.local_llm-2"));
        assert!(!valid_provider_id(""));
        assert!(!valid_provider_id("--base-url"));
        assert!(!valid_provider_id("a b"));
        assert!(!valid_provider_id("a;b"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_autostart_defaults_off_and_toggles_cleanly() {
        let test_key = format!(
            r"HKCU\Software\PraxiBotAutostartTest_{}",
            std::process::id()
        );
        assert!(!super::query_windows_autostart(&test_key));
        assert_eq!(super::write_windows_autostart(&test_key, true), Ok(true));
        assert!(super::query_windows_autostart(&test_key));
        assert_eq!(super::write_windows_autostart(&test_key, false), Ok(false));
        assert!(!super::query_windows_autostart(&test_key));
        let _ = std::process::Command::new("reg")
            .args(["delete", &test_key, "/f"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}
