//! Desktop shell library entry point for Tauri.

pub mod browser;
pub mod command_names;
pub mod control;
pub mod daemon;
pub mod listener;
mod microphone;
pub mod shell;
pub mod widget_visibility;
pub mod window_gate;

use std::sync::Arc;
use std::time::{Duration, Instant};

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
    RunEvent, State, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent,
};
use widget_visibility::WidgetVisibility;
use window_gate::{MAIN_AND_WIDGET, MAIN_ONLY, require_window};

const MENU_ID_SIMULATE_RUN: &str = "dev.simulate_bot_run";

/// Open an approved Composio URL in the operating system browser.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
async fn open_composio_link(window: Window, href: String, dashboard: bool) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    tauri::async_runtime::spawn_blocking(move || browser::open_composio(&href, dashboard))
        .await
        .map_err(|_| "system browser opener failed".to_owned())?
}
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
        sync_auxiliary_windows(&self.app, snapshot);
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

/// Heavy start-up work that used to run inside the setup hook (i.e. on the
/// main thread, before the event loop pumped): create the overlay/widget
/// webviews and start the daemon, so the UI's own `daemon_connect` finds it
/// already coming up.  Nothing here runs on, or waits for, the main thread.
fn spawn_startup_work(app: &AppHandle, daemon: Arc<DaemonHandle>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let for_windows = app.clone();
        let _ = tokio::task::spawn_blocking(move || ensure_auxiliary_windows(&for_windows)).await;
        let _ = daemon::connect(&daemon).await;
    });
}

/// The widget's visibility policy state (see `widget_visibility`), shared by every update.
type SharedWidgetVisibility = Arc<std::sync::Mutex<WidgetVisibility>>;

/// Show or hide the widget from the CURRENT control state. Must run on the main thread.
fn apply_widget_visibility(app: &AppHandle) {
    let (Some(control), Some(vis)) = (
        app.try_state::<ControlHandle>(),
        app.try_state::<SharedWidgetVisibility>(),
    ) else {
        return;
    };
    let snap = control.snapshot();
    // A poisoned lock fails open: showing the indicator is the safe direction.
    let show = vis.lock().map_or(true, |v| {
        v.visible(
            snap.mode,
            snap.stop_seq,
            snap.stop_acked_seq,
            Instant::now(),
        )
    });
    // The widget is for when the main window is not in front (see `widget_visibility`).
    let main_in_front = app.get_webview_window("main").is_some_and(|main| {
        main.is_focused().unwrap_or(false) && !main.is_minimized().unwrap_or(false)
    });
    let show = widget_visibility::presentable(show, main_in_front);
    if let Some(widget) = app.get_webview_window("widget") {
        let _ = if show { widget.show() } else { widget.hide() };
    }
}

/// After an ack the widget shows its confirmation, then hides ~4.2 s after the NEWEST
/// ack if nothing keeps it visible. An older timer, or a Stop pressed meanwhile, never
/// hides it: the decision is re-made on the main thread when the timer fires.
fn hide_widget_after_confirmation(app: &AppHandle, generation: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(widget_visibility::CONFIRM_LINGER).await;
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let (Some(control), Some(vis)) = (
                app_main.try_state::<ControlHandle>(),
                app_main.try_state::<SharedWidgetVisibility>(),
            ) else {
                return;
            };
            let snap = control.snapshot();
            let hide = vis.lock().is_ok_and(|v| {
                v.should_hide_on_timer(
                    generation,
                    snap.mode,
                    snap.stop_seq,
                    snap.stop_acked_seq,
                    Instant::now(),
                )
            });
            if let Some(widget) = app_main.get_webview_window("widget").filter(|_| hide) {
                let _ = widget.hide();
            }
        });
    });
}

fn sync_auxiliary_windows(app: &AppHandle, snapshot: &ControlSnapshot) {
    let mode = snapshot.mode;
    let app_clone = app.clone();
    let _ = app.run_on_main_thread(move || {
        ensure_auxiliary_windows(&app_clone);
        let monitors = collect_monitors_geometry(&app_clone);

        apply_widget_visibility(&app_clone);
        if mode == ControlMode::Idle {
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
        bot_shape: Some("cloud".to_string()),
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
            control.finish_run(expected_gen);
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
    window: Window,
    control: State<'_, ControlHandle>,
    rect: Option<OverlayRect>,
    label: String,
    action: Option<String>,
    text: Option<String>,
) -> Result<bool, String> {
    require_window(&window, MAIN_ONLY)?;
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
    window: Window,
    control: State<'_, ControlHandle>,
    bot: BotRunMeta,
    initial_step: String,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_ONLY)?;
    Ok(control.start_run(bot, initial_step))
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn pause_bot_run(
    window: Window,
    control: State<'_, ControlHandle>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    control.pause().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn resume_bot_run(
    window: Window,
    control: State<'_, ControlHandle>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    control.resume().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn take_the_wheel(
    window: Window,
    control: State<'_, ControlHandle>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    control.take_the_wheel().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn give_back_control(
    window: Window,
    control: State<'_, ControlHandle>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    control.give_back().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn stop_bot_run(
    window: Window,
    control: State<'_, ControlHandle>,
    token: Option<String>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    Ok(control.stop_user(token))
}

/// Normal end of a run (not a Stop): `* -> Idle` without raising `stopSeq`. A no-op when
/// `expected_generation` is not the current run generation.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn finish_bot_run(
    window: Window,
    control: State<'_, ControlHandle>,
    expected_generation: u64,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_ONLY)?;
    Ok(control.finish_run(expected_generation))
}

/// Acknowledges a handled stop sequence. Silent (no event).
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn ack_stop(
    window: Window,
    app: AppHandle,
    control: State<'_, ControlHandle>,
    visibility: State<'_, SharedWidgetVisibility>,
    seq: u64,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_ONLY)?;
    let snapshot = control.ack_stop(seq);
    let generation = visibility
        .lock()
        .map_err(|e| e.to_string())?
        .on_ack(Instant::now());
    hide_widget_after_confirmation(&app, generation);
    Ok(snapshot)
}

/// The user dismisses the widget's pending-Stop indicator (daemon unreachable, no ack
/// coming). Acknowledges nothing; the main window keeps showing the Stop's real state.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn dismiss_stop_widget(
    window: Window,
    app: AppHandle,
    control: State<'_, ControlHandle>,
    visibility: State<'_, SharedWidgetVisibility>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_AND_WIDGET)?;
    let snapshot = control.snapshot();
    visibility
        .lock()
        .map_err(|e| e.to_string())?
        .on_dismiss(snapshot.stop_seq);
    let app_main = app.clone();
    let _ = app.run_on_main_thread(move || apply_widget_visibility(&app_main));
    Ok(snapshot)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn set_stop_hotkey(
    window: Window,
    control: State<'_, ControlHandle>,
    hotkey: String,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_ONLY)?;
    control.set_stop_hotkey(hotkey).map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_show_target(
    window: Window,
    control: State<'_, ControlHandle>,
    rect: OverlayRect,
    label: String,
    action: String,
    text: Option<String>,
) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    control.show_target(rect, label, action, text);
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_click(
    window: Window,
    control: State<'_, ControlHandle>,
    x: f64,
    y: f64,
) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    control.click(x, y);
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn overlay_clear(window: Window, control: State<'_, ControlHandle>) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    control.clear();
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn simulate_bot_run(
    window: Window,
    app: AppHandle,
    control: State<'_, ControlHandle>,
    bot: Option<BotRunMeta>,
) -> Result<ControlSnapshot, String> {
    require_window(&window, MAIN_ONLY)?;
    spawn_simulated_bot_run(&app, control.inner().clone(), bot);
    Ok(control.snapshot())
}

/// `async` so it never runs on the main thread (a sync command does, and the
/// monitor query round-trips through that thread).
#[tauri::command]
async fn get_monitors_geometry(app: AppHandle) -> Vec<MonitorGeometry> {
    collect_monitors_geometry(&app)
}

// ── Daemon lifecycle / provider management (task 017) ─────────────────────────

/// Start the daemon if needed and return the base URL + bearer token.
#[tauri::command]
async fn daemon_connect(
    window: Window,
    daemon: State<'_, Arc<DaemonHandle>>,
) -> Result<Connection, String> {
    require_window(&window, MAIN_ONLY)?;
    daemon::connect(&daemon).await
}

/// The owner key of the daemon this app started, for the web view to send as
/// `x-praxi-owner-key` on owner-only requests. Main window only.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window over by value
fn owner_key(window: Window) -> Result<String, String> {
    require_window(&window, MAIN_ONLY)?;
    daemon::owner_key().map(str::to_owned)
}

/// Stop a daemon this app started, then start it again (e.g. to pick up new
/// providers).  A daemon started by someone else is left alone.
///
/// `safe`: `true` starts it in safe mode (tools, schedules, channels off), `false`
/// starts it normally again, absent keeps the current mode. This is the only way in
/// or out of safe mode while the app runs, and only the main window may ask.
#[tauri::command]
async fn daemon_restart(
    window: Window,
    daemon: State<'_, Arc<DaemonHandle>>,
    safe: Option<bool>,
) -> Result<Connection, String> {
    require_window(&window, MAIN_ONLY)?;
    daemon::restart(&daemon, safe).await
}

/// How far the local service got when it does not come up: stage, exit code, last log lines.
/// Read-only; the start-up screen polls it. Main window only.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window and state over by value
fn daemon_diagnostics(
    window: Window,
    daemon: State<'_, Arc<DaemonHandle>>,
) -> Result<daemon::Diagnostics, String> {
    require_window(&window, MAIN_ONLY)?;
    Ok(daemon::diagnostics(&daemon))
}

/// Show the folder with `desktop-daemon.log` in the file manager. The path is the app's own data
/// directory, never an argument. Main window only.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window over by value
fn open_log_folder(window: Window) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    let dir = daemon::log_folder()?;
    open::that(&dir).map_err(|e| format!("cannot open {} ({e})", dir.display()))
}

/// Arguments of `providers_cli` (typed so no free-form flag reaches the CLI).
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProvidersAction {
    List,
    Add,
    Test,
    Default,
    DetectLocal,
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
    window: Window,
    action: ProvidersAction,
    id: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
) -> Result<CliOutput, String> {
    require_window(&window, MAIN_ONLY)?;
    let mut args: Vec<String> = Vec::new();
    let mut key = None;
    if let ProvidersAction::List = action {
        args.push("list".into());
    } else if let ProvidersAction::DetectLocal = action {
        args.push("detect-local".into());
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
            ProvidersAction::List | ProvidersAction::DetectLocal => {}
        }
    }
    daemon::run_providers_cli(args, key).await
}

#[cfg(windows)]
const DEFAULT_AUTOSTART_REG_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const AUTOSTART_VALUE_NAME: &str = "Praxi-Bot";

#[cfg(windows)]
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
        if !query_windows_autostart(reg_key) {
            return Err("autostart was not enabled (registry read-back failed)".into());
        }
        Ok(true)
    } else {
        if query_windows_autostart(reg_key) {
            let status = std::process::Command::new("reg")
                .args(["delete", reg_key, "/v", AUTOSTART_VALUE_NAME, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| format!("cannot update autostart registry entry: {e}"))?;
            if !status.success() {
                return Err("failed to disable Windows autostart".into());
            }
        }
        // Read back so a silent no-op is reported instead of trusted.
        if query_windows_autostart(reg_key) {
            return Err("autostart is still enabled after disabling it".into());
        }
        Ok(false)
    }
}

/// Check whether per-user autostart (`HKCU\...\Run\Praxi-Bot`) is enabled.
/// Off by default.
///
/// `async` + `spawn_blocking`: the lookup spawns `reg.exe`, which must not stall
/// the main (UI) thread while the app is starting.
#[tauri::command]
async fn get_autostart(window: Window) -> Result<bool, String> {
    require_window(&window, MAIN_ONLY)?;
    tokio::task::spawn_blocking(get_autostart_impl)
        .await
        .map_err(|e| e.to_string())
}

fn get_autostart_impl() -> bool {
    #[cfg(windows)]
    {
        query_windows_autostart(&autostart_reg_key())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Whether this platform can register per-user autostart (Windows only).
fn autostart_supported_impl() -> bool {
    cfg!(windows)
}

/// Whether autostart can be toggled on this platform (the UI disables the
/// toggle when this is false).
#[tauri::command]
async fn autostart_supported() -> bool {
    autostart_supported_impl()
}

fn set_autostart_impl(enabled: bool) -> Result<bool, String> {
    if !autostart_supported_impl() {
        let _ = enabled;
        return Err("autostart is not supported on this platform".into());
    }
    #[cfg(windows)]
    {
        write_windows_autostart(&autostart_reg_key(), enabled)
    }
    #[cfg(not(windows))]
    {
        unreachable!("checked by autostart_supported_impl")
    }
}

/// Enable or disable per-user Windows autostart (`HKCU\...\Run\Praxi-Bot`).
///
/// # Errors
/// Off Windows, or when the registry write/read-back disagrees with `enabled`.
#[tauri::command]
async fn set_autostart(window: Window, enabled: bool) -> Result<bool, String> {
    require_window(&window, MAIN_ONLY)?;
    tokio::task::spawn_blocking(move || set_autostart_impl(enabled))
        .await
        .map_err(|e| e.to_string())?
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
    let daemon_for_setup = Arc::clone(&daemon_handle);

    let builder = tauri::Builder::default();
    // A second launch (Start menu, autostart plus a manual start, deep link)
    // must not start another daemon and tray: it focuses the running window
    // instead. This plugin has to be registered before every other one.
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        shell::focus_main(app);
    }));
    let app = builder
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .manage(shell::TrayState::default())
        .manage(control)
        .manage(SharedWidgetVisibility::default())
        .manage(daemon_handle)
        .setup(move |app| {
            let app_handle = app.handle().clone();
            #[cfg(windows)]
            microphone::install(&app_handle)?;
            control_for_setup.set_sink(Arc::new(TauriOverlaySink {
                app: app_handle.clone(),
            }));

            // Overlay + widget webviews (WebView2 start-up, one per monitor) are
            // built off the main thread so the main window paints and answers
            // the OS from its first frame; the daemon starts in parallel.
            spawn_startup_work(&app_handle, Arc::clone(&daemon_for_setup));
            spawn_global_hotkey_listener(control_for_setup.clone());
            setup_dev_menu(app, control_for_setup.clone());
            if let Err(e) = shell::setup_tray(&app_handle) {
                eprintln!("bot-desktop: tray icon unavailable: {e}");
            }

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
            if window.label() == "main" && matches!(event, WindowEvent::Focused(_)) {
                // Leaving the main window shows the widget (while a run is active); coming
                // back hides it again, so it never sits on top of the chat or right panel.
                apply_widget_visibility(window.app_handle());
            }
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
            open_composio_link,
            get_control_state,
            is_human_driving,
            begin_action,
            start_bot_run,
            pause_bot_run,
            resume_bot_run,
            take_the_wheel,
            give_back_control,
            stop_bot_run,
            finish_bot_run,
            ack_stop,
            dismiss_stop_widget,
            set_stop_hotkey,
            overlay_show_target,
            overlay_click,
            overlay_clear,
            simulate_bot_run,
            get_monitors_geometry,
            daemon_connect,
            owner_key,
            daemon_restart,
            daemon_diagnostics,
            open_log_folder,
            providers_cli,
            get_autostart,
            set_autostart,
            autostart_supported,
            shell::notify_show,
            shell::notify_take_actions,
            shell::set_needs_count,
            shell::set_tray_labels,
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

    /// Sync `#[tauri::command]`s run on the main thread: the ones that spawn a
    /// process or query monitors must stay `async` (startup "not responding").
    #[test]
    fn blocking_commands_stay_off_the_main_thread() {
        let src = include_str!("lib.rs");
        for name in [
            "get_autostart",
            "set_autostart",
            "autostart_supported",
            "get_monitors_geometry",
        ] {
            assert!(
                src.contains(&format!("async fn {name}(")),
                "`{name}` must be an async command"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn autostart_is_unsupported_off_windows() {
        assert!(!super::autostart_supported_impl());
        assert!(!super::get_autostart_impl());
        assert_eq!(
            super::set_autostart_impl(true),
            Err("autostart is not supported on this platform".to_owned())
        );
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
        // Disabling a missing value is fine.
        assert_eq!(super::write_windows_autostart(&test_key, false), Ok(false));
        let _ = std::process::Command::new("reg")
            .args(["delete", &test_key, "/f"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Card 239: every custom command is listed in the app manifest, the manifest
/// is really handed to `tauri-build`, the capability files grant each window
/// exactly its documented set, and the commands that must check the calling
/// window do so.
#[cfg(test)]
mod acl_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::command_names::COMMAND_NAMES;

    fn desktop_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn tauri_conf() -> serde_json::Value {
        serde_json::from_str(&read(&desktop_dir().join("tauri.conf.json")))
            .expect("tauri.conf.json")
    }

    /// Every `*.rs` file under `dir`, recursively.
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    fn source_files() -> Vec<PathBuf> {
        let mut files = Vec::new();
        rust_files(&desktop_dir().join("src"), &mut files);
        files.sort();
        files
    }

    /// Function names that follow a `#[tauri::command...]` attribute in `src/**/*.rs`,
    /// each with the source of its body (up to the next top-level `}`).
    fn command_fns_in_sources() -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for path in source_files() {
            let text = read(&path).replace("\r\n", "\n");
            let lines: Vec<&str> = text.lines().collect();
            for (n, line) in lines.iter().enumerate() {
                // Only a real attribute line, not this test's own string literals.
                if !line.trim_start().starts_with("#[tauri::command") {
                    continue;
                }
                let rest = lines[n + 1..].join(
                    "
",
                );
                let fn_at = rest.find("fn ").expect("fn after #[tauri::command]");
                let signature = &rest[fn_at + 3..];
                let name = signature
                    .split(['(', '<'])
                    .next()
                    .expect("fn name")
                    .trim()
                    .to_owned();
                let end = signature
                    .find(
                        "
}
",
                    )
                    .map_or(signature.len(), |i| i + 3);
                found.insert(name, signature[..end].to_owned());
            }
        }
        found
    }

    /// Names inside `tauri::generate_handler![ ... ]` (module prefixes stripped).
    fn registered_handlers() -> BTreeSet<String> {
        let text = read(&desktop_dir().join("src").join("lib.rs"));
        let start =
            text.find("generate_handler![").expect("handler list") + "generate_handler![".len();
        let end = start + text[start..].find(']').expect("end of handler list");
        text[start..end]
            .split(',')
            .map(|item| item.trim().rsplit("::").next().unwrap_or("").to_owned())
            .filter(|item| !item.is_empty())
            .collect()
    }

    fn manifest() -> BTreeSet<String> {
        COMMAND_NAMES.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn build_rs_hands_the_command_list_to_the_app_manifest() {
        // If build.rs went back to plain `tauri_build::build()`, Tauri would not
        // create the app ACL at all and would silently allow every custom command
        // from every window, with all other tests still green.
        let build_rs = read(&desktop_dir().join("build.rs")).replace(char::is_whitespace, "");
        assert!(
            build_rs.contains("include!(\"src/command_names.rs\");"),
            "build.rs must include src/command_names.rs"
        );
        assert!(
            build_rs.contains("tauri_build::try_build("),
            "build.rs must call tauri_build::try_build"
        );
        assert!(
            build_rs.contains("AppManifest::new().commands(COMMAND_NAMES)"),
            "build.rs must pass COMMAND_NAMES to AppManifest::commands"
        );
        assert!(
            !build_rs.contains("tauri_build::build()"),
            "plain tauri_build::build() has no app manifest"
        );
    }

    #[test]
    fn every_registered_command_has_a_manifest_entry() {
        let declared: BTreeSet<String> = command_fns_in_sources().into_keys().collect();
        let registered = registered_handlers();
        let manifest = manifest();
        assert_eq!(
            COMMAND_NAMES.len(),
            manifest.len(),
            "duplicate entry in src/command_names.rs"
        );
        let missing: Vec<_> = registered.difference(&manifest).collect();
        assert!(
            missing.is_empty(),
            "registered in generate_handler! but missing from COMMAND_NAMES \
             (src/command_names.rs), so no window could call it: {missing:?}"
        );
        let stale: Vec<_> = manifest.difference(&registered).collect();
        assert!(
            stale.is_empty(),
            "in COMMAND_NAMES but not registered: {stale:?}"
        );
        let unregistered: Vec<_> = declared.difference(&registered).collect();
        assert!(
            unregistered.is_empty(),
            "#[tauri::command] fns missing from generate_handler!: {unregistered:?}"
        );
        let undeclared: Vec<_> = registered.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "registered handlers without #[tauri::command]: {undeclared:?}"
        );
    }

    /// Commands that must reject a call from a window they are not meant for,
    /// with the allow-list constant they must check. Everything else must be
    /// listed in `ACL_ONLY`, so a new command has to be classified here.
    const GATED: &[(&str, &str)] = &[
        ("open_composio_link", "MAIN_ONLY"),
        ("begin_action", "MAIN_ONLY"),
        ("start_bot_run", "MAIN_ONLY"),
        ("pause_bot_run", "MAIN_AND_WIDGET"),
        ("resume_bot_run", "MAIN_AND_WIDGET"),
        ("take_the_wheel", "MAIN_AND_WIDGET"),
        ("give_back_control", "MAIN_AND_WIDGET"),
        ("stop_bot_run", "MAIN_AND_WIDGET"),
        ("finish_bot_run", "MAIN_ONLY"),
        ("ack_stop", "MAIN_ONLY"),
        ("dismiss_stop_widget", "MAIN_AND_WIDGET"),
        ("set_stop_hotkey", "MAIN_ONLY"),
        ("overlay_show_target", "MAIN_ONLY"),
        ("overlay_click", "MAIN_ONLY"),
        ("overlay_clear", "MAIN_ONLY"),
        ("simulate_bot_run", "MAIN_ONLY"),
        ("daemon_connect", "MAIN_ONLY"),
        ("owner_key", "MAIN_ONLY"),
        ("daemon_restart", "MAIN_ONLY"),
        ("daemon_diagnostics", "MAIN_ONLY"),
        ("open_log_folder", "MAIN_ONLY"),
        ("providers_cli", "MAIN_ONLY"),
        ("get_autostart", "MAIN_ONLY"),
        ("set_autostart", "MAIN_ONLY"),
        ("notify_show", "MAIN_ONLY"),
        ("notify_take_actions", "MAIN_ONLY"),
        ("set_needs_count", "MAIN_ONLY"),
        ("set_tray_labels", "MAIN_ONLY"),
    ];

    /// Read-only commands with no side effect beyond returning state; the
    /// capability files are their only gate (and `get_control_state` is the
    /// one command the widget and overlays use to draw).
    const ACL_ONLY: &[&str] = &[
        "get_control_state",
        "is_human_driving",
        "get_monitors_geometry",
        "autostart_supported",
    ];

    #[test]
    fn every_gated_command_calls_the_window_gate() {
        let commands = command_fns_in_sources();
        let classified: BTreeSet<String> = GATED
            .iter()
            .map(|(n, _)| (*n).to_owned())
            .chain(ACL_ONLY.iter().map(|n| (*n).to_owned()))
            .collect();
        assert_eq!(
            classified,
            commands.keys().cloned().collect::<BTreeSet<_>>(),
            "every command must be listed in GATED or ACL_ONLY"
        );
        for (name, allow) in GATED {
            let body = &commands[*name];
            let call = format!("require_window(&window, {allow})?;");
            let body_flat = body.replace("crate::window_gate::", "");
            assert!(
                body_flat.contains(&call),
                "{name} must call {call} before doing anything"
            );
            assert!(
                body_flat.contains("window: Window") || body_flat.contains("window: tauri::Window"),
                "{name} must take the calling window"
            );
        }
    }

    /// Enabled capability -> (id, window patterns, custom commands it allows).
    /// Only `allow-*` permissions without a plugin prefix are app commands.
    fn app_grants() -> Vec<(String, Vec<String>, BTreeSet<String>)> {
        let config = tauri_conf();
        let enabled: Vec<String> = config["app"]["security"]["capabilities"]
            .as_array()
            .expect("capabilities list")
            .iter()
            .map(|v| v.as_str().expect("capability id").to_owned())
            .collect();
        let mut out = Vec::new();
        for entry in fs::read_dir(desktop_dir().join("capabilities")).expect("capabilities dir") {
            let path = entry.expect("dir entry").path();
            let cap: serde_json::Value =
                serde_json::from_str(&read(&path)).expect("capability json");
            let id = cap["identifier"].as_str().expect("identifier").to_owned();
            if !enabled.contains(&id) {
                continue;
            }
            let windows = cap["windows"]
                .as_array()
                .map(|w| {
                    w.iter()
                        .map(|v| v.as_str().expect("window").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let commands = cap["permissions"]
                .as_array()
                .expect("permissions")
                .iter()
                .filter_map(|p| {
                    let id = p.as_str().or_else(|| p["identifier"].as_str())?;
                    id.strip_prefix("allow-").map(|c| c.replace('-', "_"))
                })
                .collect();
            out.push((id, windows, commands));
        }
        let ids: BTreeSet<_> = out.iter().map(|(id, _, _)| id.clone()).collect();
        for want in &enabled {
            assert!(
                ids.contains(want),
                "tauri.conf.json enables unknown capability {want}"
            );
        }
        out
    }

    /// Window label patterns are an exact label or a `prefix*` glob.
    fn pattern_matches(pattern: &str, label: &str) -> bool {
        match pattern.strip_suffix('*') {
            Some(prefix) => !prefix.contains('*') && label.starts_with(prefix),
            None => pattern == label,
        }
    }

    fn commands_for(label: &str) -> BTreeSet<String> {
        let mut allowed = BTreeSet::new();
        for (_, windows, commands) in app_grants() {
            if windows.iter().any(|p| pattern_matches(p, label)) {
                allowed.extend(commands);
            }
        }
        allowed
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// Manifest commands no window calls today (no UI caller). They keep an
    /// `allow-*` permission in the manifest but are granted to no window.
    const UNGRANTED: &[&str] = &[
        "is_human_driving",
        "overlay_show_target",
        "overlay_clear",
        "get_monitors_geometry",
        "autostart_supported",
    ];

    #[test]
    fn each_window_label_may_call_exactly_its_documented_commands() {
        let main: BTreeSet<String> = manifest().difference(&set(UNGRANTED)).cloned().collect();
        let expected: BTreeMap<&str, BTreeSet<String>> = BTreeMap::from([
            ("main", main),
            (
                "widget",
                set(&[
                    "get_control_state",
                    "pause_bot_run",
                    "resume_bot_run",
                    "take_the_wheel",
                    "give_back_control",
                    "stop_bot_run",
                    "dismiss_stop_widget",
                ]),
            ),
            ("overlay-0", set(&["get_control_state"])),
            ("overlay-3", set(&["get_control_state"])),
            ("some-future-window", BTreeSet::new()),
            ("", BTreeSet::new()),
        ]);
        for (label, want) in expected {
            assert_eq!(
                commands_for(label),
                want,
                "commands callable from window {label:?}"
            );
        }
        // finish_bot_run / ack_stop are reserved for card 237 PR C and main only.
        assert!(commands_for("main").contains("finish_bot_run"));
        assert!(commands_for("main").contains("ack_stop"));
    }

    #[test]
    fn capabilities_only_reference_manifest_commands_and_use_simple_window_patterns() {
        let manifest = manifest();
        for (id, windows, commands) in app_grants() {
            for c in &commands {
                assert!(
                    manifest.contains(c),
                    "capability {id} allows unknown command {c}"
                );
            }
            for w in &windows {
                assert!(
                    w.matches('*').count() == usize::from(w.ends_with('*')),
                    "capability {id}: window pattern {w:?} must be an exact label or end in *"
                );
            }
        }
    }

    #[test]
    fn widget_and_overlays_cannot_reach_the_sensitive_commands() {
        for label in ["widget", "overlay-0"] {
            let allowed = commands_for(label);
            for sensitive in [
                "start_bot_run",
                "finish_bot_run",
                "ack_stop",
                "begin_action",
                "providers_cli",
                "get_autostart",
                "set_autostart",
                "daemon_connect",
                "owner_key",
                "daemon_restart",
                "daemon_diagnostics",
                "open_log_folder",
                "simulate_bot_run",
                "set_stop_hotkey",
                "notify_show",
                "notify_take_actions",
                "set_needs_count",
                "set_tray_labels",
            ] {
                assert!(
                    !allowed.contains(sensitive),
                    "{label} must not call {sensitive}"
                );
            }
        }
    }

    /// Every `http(s)://` / `ws(s)://` origin in a CSP string.
    fn origins(csp: &str) -> Vec<String> {
        csp.split([' ', ';'])
            .filter(|t| {
                ["http://", "https://", "ws://", "wss://"]
                    .iter()
                    .any(|p| t.starts_with(p))
            })
            .map(str::to_owned)
            .collect()
    }

    fn is_loopback_origin(origin: &str) -> bool {
        let host = origin.split("://").nth(1).unwrap_or("");
        let host = host
            .strip_prefix("[::1]")
            .map_or_else(|| host.split(':').next().unwrap_or(""), |_| "[::1]");
        matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "ipc.localhost")
    }

    #[test]
    fn csp_is_set_and_has_no_remote_scripts_or_eval() {
        let config = tauri_conf();
        let csp = config["app"]["security"]["csp"]
            .as_str()
            .expect("csp must be a string, not null");
        assert_eq!(
            csp,
            "default-src 'self'; script-src 'self'; style-src 'self'; \
             style-src-attr 'unsafe-inline'; img-src 'self' blob: data:; \
             font-src 'self' data:; \
             connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:* \
             http://localhost:* http://[::1]:*; \
             object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"
        );
        assert!(!csp.contains("unsafe-eval"));
        let script_src = csp
            .split("; ")
            .find(|d| d.starts_with("script-src "))
            .expect("script-src directive");
        assert_eq!(
            script_src, "script-src 'self'",
            "no inline or remote scripts in release"
        );
        for origin in origins(csp) {
            assert!(is_loopback_origin(&origin), "release CSP origin {origin}");
        }
    }

    #[test]
    fn dev_csp_allows_only_loopback_origins_and_no_eval() {
        let config = tauri_conf();
        let dev = config["app"]["security"]["devCsp"]
            .as_str()
            .expect("devCsp");
        assert!(!dev.contains("unsafe-eval"));
        assert!(!dev.contains("https:"), "dev CSP must not allow https:");
        let found = origins(dev);
        assert!(found.iter().any(|o| o.starts_with("ws://localhost:1420")));
        for origin in found {
            assert!(
                is_loopback_origin(&origin),
                "devCsp allows a non-loopback origin: {origin}"
            );
        }
        // Bare scheme sources would allow any host.
        for scheme in ["http:", "ws:", "wss:", "*"] {
            assert!(
                !dev.split([' ', ';']).any(|t| t == scheme),
                "devCsp must not contain the bare source {scheme}"
            );
        }
    }

    #[test]
    fn windows_keep_the_default_http_ipc_scheme_the_csp_assumes() {
        // The CSP allows `http://ipc.localhost` (Windows IPC with useHttpsScheme
        // false). Turning useHttpsScheme on would move IPC to https://ipc.localhost
        // and the CSP would block every command; change both together.
        let config = tauri_conf();
        for window in config["app"]["windows"].as_array().expect("windows") {
            assert_ne!(window["useHttpsScheme"], serde_json::json!(true));
        }
    }
}
