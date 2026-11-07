/**
 * Overlay and control event interface for Praxi-Bot desktop.
 *
 * Coordinate Convention:
 * - Matching `crates/desktop-api` (`CoordinateSpace::Absolute` and `desktop_api::snapshot::Rect`),
 *   all coordinates in `overlay.show_target {rect, label, action}` and `overlay.click {x, y}`
 *   are expressed in **global physical screen pixels**.
 * - Each per-monitor overlay window converts incoming global physical coordinates into its own
 *   monitor-local logical CSS pixels using `physicalRectToLogical` and `physicalPointToLogical`:
 *     logicalX = (physicalX - monitor.physicalX) / monitor.scaleFactor
 *     logicalY = (physicalY - monitor.physicalY) / monitor.scaleFactor
 */

import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import type { AppSettings } from "../store/settingsStore";

export const OVERLAY_SHOW_TARGET_EVENT = "overlay.show_target" as const;
export const OVERLAY_CLICK_EVENT = "overlay.click" as const;
export const OVERLAY_CLEAR_EVENT = "overlay.clear" as const;
export const CONTROL_STATE_CHANGED_EVENT = "control.state_changed" as const;

const WIRE_OVERLAY_SHOW_TARGET = "overlay:show_target";
const WIRE_OVERLAY_CLICK = "overlay:click";
const WIRE_OVERLAY_CLEAR = "overlay:clear";
const WIRE_CONTROL_STATE = "control:state_changed";
const WIRE_SETTINGS_UPDATED = "settings:updated";

export type ControlMode = "bot_driving" | "human_driving" | "paused" | "idle";

export interface OverlayRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface OverlayShowTargetPayload {
  rect: OverlayRect;
  label: string;
  action: string;
  text?: string;
}

export interface OverlayClickPayload {
  x: number;
  y: number;
  timestampMs?: number;
}

export interface BotRunMeta {
  botId: string;
  botName: string;
  botAvatar?: string;
  botColor?: string;
  botInitials?: string;
}

export interface ControlSnapshot {
  mode: ControlMode;
  runGeneration: number;
  bot: BotRunMeta | null;
  currentStep: string | null;
  stopHotkey: string;
  activeTarget: OverlayShowTargetPayload | null;
  lastClick: OverlayClickPayload | null;
  refusedActionsCount: number;
}

export interface MonitorGeometry {
  monitorIndex: number;
  physicalX: number;
  physicalY: number;
  physicalWidth: number;
  physicalHeight: number;
  scaleFactor: number;
}

export const INITIAL_CONTROL_SNAPSHOT: ControlSnapshot = {
  mode: "idle",
  runGeneration: 0,
  bot: null,
  currentStep: null,
  stopHotkey: DEFAULT_STOP_HOTKEY,
  activeTarget: null,
  lastClick: null,
  refusedActionsCount: 0
};

export function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

/**
 * Reads monitor geometry passed via URL query parameters by the Tauri window manager,
 * falling back to `window.screen` and `window.devicePixelRatio`.
 */
export function readMonitorGeometryFromUrl(): MonitorGeometry {
  if (typeof window === "undefined") {
    return {
      monitorIndex: 0,
      physicalX: 0,
      physicalY: 0,
      physicalWidth: 1920,
      physicalHeight: 1080,
      scaleFactor: 1
    };
  }

  const params = new URLSearchParams(window.location.search);
  const dpr = window.devicePixelRatio || 1;
  const monitorIndex = Number(params.get("monitorIndex") ?? "0") || 0;
  const physicalX = Number(params.get("physX") ?? "0") || 0;
  const physicalY = Number(params.get("physY") ?? "0") || 0;
  const physicalWidth =
    Number(params.get("physW") ?? "0") || Math.round(window.innerWidth * dpr);
  const physicalHeight =
    Number(params.get("physH") ?? "0") || Math.round(window.innerHeight * dpr);
  const scaleFactor = Number(params.get("scale") ?? "0") || dpr;

  return {
    monitorIndex,
    physicalX,
    physicalY,
    physicalWidth,
    physicalHeight,
    scaleFactor: scaleFactor > 0 ? scaleFactor : 1
  };
}

/**
 * Converts a rectangle in global physical screen pixels to monitor-local logical CSS pixels.
 */
export function physicalRectToLogical(
  rect: OverlayRect,
  monitor: MonitorGeometry
): OverlayRect {
  const scale =
    Number.isFinite(monitor.scaleFactor) && monitor.scaleFactor > 0
      ? monitor.scaleFactor
      : 1;
  return {
    x: (rect.x - monitor.physicalX) / scale,
    y: (rect.y - monitor.physicalY) / scale,
    width: rect.width / scale,
    height: rect.height / scale
  };
}

/**
 * Converts a point in global physical screen pixels to monitor-local logical CSS pixels.
 */
export function physicalPointToLogical(
  point: { x: number; y: number },
  monitor: MonitorGeometry
): { x: number; y: number } {
  const scale =
    Number.isFinite(monitor.scaleFactor) && monitor.scaleFactor > 0
      ? monitor.scaleFactor
      : 1;
  return {
    x: (point.x - monitor.physicalX) / scale,
    y: (point.y - monitor.physicalY) / scale
  };
}

/**
 * Extracts typed-text preview from an `OverlayShowTargetPayload` if the action is a typing step.
 */
export function extractTypedTextPreview(
  target: OverlayShowTargetPayload | null
): string | null {
  if (!target) {
    return null;
  }
  if (typeof target.text === "string" && target.text.trim().length > 0) {
    return target.text;
  }
  const normalized = target.action.trim();
  if (normalized.toLowerCase().startsWith("type:")) {
    const body = normalized.slice(5).trim();
    return body.length > 0 ? body : null;
  }
  return null;
}

export interface OverlayEventHandlers {
  onShowTarget?: (payload: OverlayShowTargetPayload) => void;
  onClick?: (payload: OverlayClickPayload) => void;
  onClear?: () => void;
  onControlStateChanged?: (snapshot: ControlSnapshot) => void;
  onSettingsUpdated?: (settings: AppSettings) => void;
}

/**
 * Subscribes to `overlay.show_target`, `overlay.click`, `overlay.clear`, and `control.state_changed`
 * across both Tauri IPC and DOM CustomEvents.
 */
export function subscribeOverlayEvents(
  handlers: OverlayEventHandlers
): () => void {
  let disposed = false;
  const unlistens: UnlistenFn[] = [];

  const handleDomShowTarget = (event: Event) => {
    const custom = event as CustomEvent<OverlayShowTargetPayload>;
    if (custom.detail) {
      handlers.onShowTarget?.(custom.detail);
    }
  };
  const handleDomClick = (event: Event) => {
    const custom = event as CustomEvent<OverlayClickPayload>;
    if (custom.detail) {
      handlers.onClick?.(custom.detail);
    }
  };
  const handleDomClear = () => {
    handlers.onClear?.();
  };
  const handleDomControl = (event: Event) => {
    const custom = event as CustomEvent<ControlSnapshot>;
    if (custom.detail) {
      handlers.onControlStateChanged?.(custom.detail);
    }
  };

  if (typeof window !== "undefined") {
    window.addEventListener(OVERLAY_SHOW_TARGET_EVENT, handleDomShowTarget);
    window.addEventListener(OVERLAY_CLICK_EVENT, handleDomClick);
    window.addEventListener(OVERLAY_CLEAR_EVENT, handleDomClear);
    window.addEventListener(CONTROL_STATE_CHANGED_EVENT, handleDomControl);
  }

  if (isTauriRuntime()) {
    void listen<OverlayShowTargetPayload>(WIRE_OVERLAY_SHOW_TARGET, (e) => {
      if (disposed) return;
      if (typeof window !== "undefined") {
        window.dispatchEvent(
          new CustomEvent(OVERLAY_SHOW_TARGET_EVENT, { detail: e.payload })
        );
      } else {
        handlers.onShowTarget?.(e.payload);
      }
    }).then((u) => {
      if (disposed) u();
      else unlistens.push(u);
    });

    void listen<OverlayClickPayload>(WIRE_OVERLAY_CLICK, (e) => {
      if (disposed) return;
      if (typeof window !== "undefined") {
        window.dispatchEvent(
          new CustomEvent(OVERLAY_CLICK_EVENT, { detail: e.payload })
        );
      } else {
        handlers.onClick?.(e.payload);
      }
    }).then((u) => {
      if (disposed) u();
      else unlistens.push(u);
    });

    void listen(WIRE_OVERLAY_CLEAR, () => {
      if (disposed) return;
      if (typeof window !== "undefined") {
        window.dispatchEvent(new CustomEvent(OVERLAY_CLEAR_EVENT));
      } else {
        handlers.onClear?.();
      }
    }).then((u) => {
      if (disposed) u();
      else unlistens.push(u);
    });

    void listen<ControlSnapshot>(WIRE_CONTROL_STATE, (e) => {
      if (disposed) return;
      if (typeof window !== "undefined") {
        window.dispatchEvent(
          new CustomEvent(CONTROL_STATE_CHANGED_EVENT, { detail: e.payload })
        );
      } else {
        handlers.onControlStateChanged?.(e.payload);
      }
    }).then((u) => {
      if (disposed) u();
      else unlistens.push(u);
    });

    if (handlers.onSettingsUpdated) {
      void listen<AppSettings>(WIRE_SETTINGS_UPDATED, (e) => {
        if (!disposed) {
          handlers.onSettingsUpdated?.(e.payload);
        }
      }).then((u) => {
        if (disposed) u();
        else unlistens.push(u);
      });
    }
  }

  return () => {
    disposed = true;
    for (const u of unlistens) {
      u();
    }
    if (typeof window !== "undefined") {
      window.removeEventListener(
        OVERLAY_SHOW_TARGET_EVENT,
        handleDomShowTarget
      );
      window.removeEventListener(OVERLAY_CLICK_EVENT, handleDomClick);
      window.removeEventListener(OVERLAY_CLEAR_EVENT, handleDomClear);
      window.removeEventListener(
        CONTROL_STATE_CHANGED_EVENT,
        handleDomControl
      );
    }
  };
}

// ── Tauri Command Invokers ────────────────────────────────────────────────────

export async function getControlState(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  try {
    return await invoke<ControlSnapshot>("get_control_state");
  } catch {
    return INITIAL_CONTROL_SNAPSHOT;
  }
}

export async function isHumanDriving(): Promise<boolean> {
  if (!isTauriRuntime()) {
    return false;
  }
  try {
    return await invoke<boolean>("is_human_driving");
  } catch {
    return false;
  }
}

export async function beginAction(
  rect: OverlayRect,
  label: string,
  action = "click",
  text?: string
): Promise<boolean> {
  if (!isTauriRuntime()) {
    return false;
  }
  return invoke<boolean>("begin_action", { rect, label, action, text });
}

export async function pauseBotRun(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("pause_bot_run");
}

export async function resumeBotRun(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("resume_bot_run");
}

export async function takeTheWheel(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("take_the_wheel");
}

export async function giveBackControl(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("give_back_control");
}

export async function stopBotRun(): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("stop_bot_run");
}

export async function setStopHotkey(hotkey: string): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return { ...INITIAL_CONTROL_SNAPSHOT, stopHotkey: hotkey };
  }
  return invoke<ControlSnapshot>("set_stop_hotkey", { hotkey });
}

export async function simulateBotRun(
  bot?: BotRunMeta
): Promise<ControlSnapshot> {
  if (!isTauriRuntime()) {
    return INITIAL_CONTROL_SNAPSHOT;
  }
  return invoke<ControlSnapshot>("simulate_bot_run", { bot });
}

export async function broadcastSettingsUpdated(
  settings: AppSettings
): Promise<void> {
  if (!isTauriRuntime()) {
    return;
  }
  try {
    await emit(WIRE_SETTINGS_UPDATED, settings);
  } catch {
    // Ignore broadcast errors outside Tauri
  }
}

// ── Keyboard Shortcut Helpers ─────────────────────────────────────────────────

export function formatKeyboardEventAsHotkey(
  event: KeyboardEvent
): string | null {
  const ignoredKeys = new Set(["Control", "Alt", "Shift", "Meta", "OS"]);
  if (ignoredKeys.has(event.key)) {
    return null;
  }

  const hasModifier =
    event.ctrlKey || event.altKey || event.shiftKey || event.metaKey;
  if (!hasModifier) {
    return null;
  }

  let keyToken: string | null = null;
  if (event.key === "Escape" || event.key === "Esc") {
    keyToken = "Esc";
  } else if (event.key === " ") {
    keyToken = "Space";
  } else if (event.key === "Backspace") {
    keyToken = "Backspace";
  } else if (/^[a-zA-Z0-9]$/.test(event.key)) {
    keyToken = event.key.toUpperCase();
  } else if (/^F([1-9]|1[0-2])$/i.test(event.key)) {
    keyToken = event.key.toUpperCase();
  }

  if (!keyToken) {
    return null;
  }

  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Super");
  parts.push(keyToken);

  return parts.join("+");
}

export function matchesHotkeyEvent(
  event: KeyboardEvent,
  hotkeySpec: string
): boolean {
  const formatted = formatKeyboardEventAsHotkey(event);
  if (!formatted) {
    return false;
  }
  return formatted.toLowerCase() === hotkeySpec.trim().toLowerCase();
}
