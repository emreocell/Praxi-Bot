import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "../overlay/events";
import { isMockMode, MOCK_BASE_URL } from "./mockMode";

/** What the shell hands us after starting / finding the daemon. */
export interface Connection {
  base_url: string;
  token: string;
  started_by_app: boolean;
  fake_provider: boolean;
  /** Whether the app starts its service in safe mode (tools, schedules, channels off). */
  safe_mode: boolean;
  data_dir: string;
}

/**
 * Ask the Tauri shell to find or start `bot serve` and return its URL + token.
 *
 * Outside Tauri (plain `vite` in a browser, dev only) the connection comes from
 * `?daemon=<url>&token=<token>[&fake=1]` so the UI can be exercised against a daemon
 * started by hand.
 */
export async function connectDaemon(restart = false, safe?: boolean): Promise<Connection> {
  if (isMockMode()) {
    // Review state: the daemon cannot be reached (`?mock=1&screen=offline`).
    if (new URLSearchParams(window.location.search).get("screen") === "offline") {
      throw new Error("connect ECONNREFUSED 127.0.0.1:47821");
    }
    return {
      base_url: MOCK_BASE_URL,
      token: "mock",
      started_by_app: false,
      fake_provider: true,
      safe_mode: new URLSearchParams(window.location.search).get("safe") === "1",
      data_dir: ""
    };
  }
  if (isTauriRuntime()) {
    return invoke<Connection>(restart ? "daemon_restart" : "daemon_connect", restart ? { safe } : undefined);
  }
  if (import.meta.env.DEV) {
    const q = new URLSearchParams(window.location.search);
    const base = q.get("daemon");
    const token = q.get("token");
    if (base && token) {
      return {
        base_url: base,
        token,
        started_by_app: false,
        fake_provider: q.get("fake") === "1",
        safe_mode: false,
        data_dir: ""
      };
    }
  }
  throw new Error("daemon_unavailable_outside_tauri");
}

/** How far the shell's start of the local service got (mirrors `daemon::Diagnostics` in the app). */
export type StartupStage =
  | "idle"
  | "spawning"
  | "waiting"
  | "ready"
  | "sidecar_missing"
  | "sidecar_blocked"
  | "spawn_failed"
  | "exited"
  | "not_responding"
  | "port_in_use"
  | "unreachable";

export interface StartupDiagnostics {
  stage: StartupStage;
  elapsed_ms: number;
  port: number;
  base_url: string;
  data_dir: string;
  log_path: string;
  sidecar: string;
  sidecar_exists: boolean;
  pid: number | null;
  exit_code: number | null;
  error: string | null;
  log_tail: string[];
  app_version: string;
  os: string;
}

/** What the shell knows about the start of the local service; `null` outside the app. */
export async function startupDiagnostics(): Promise<StartupDiagnostics | null> {
  if (isMockMode() || !isTauriRuntime()) {
    return null;
  }
  try {
    return await invoke<StartupDiagnostics>("daemon_diagnostics");
  } catch {
    return null;
  }
}

/** What the single primary button of the first-run check does (mirrors `prereq::Fix` in the app). */
export type PrereqFix = "retry" | "open_data_folder" | "download_webview2";

export interface PrereqIssue {
  id: string;
  fix: PrereqFix;
  /** `true`: the app cannot work until fixed; `false`: advice only. */
  blocking: boolean;
}

export interface PrereqReport {
  issues: PrereqIssue[];
  /** Technical details with the user's folder and account names already removed. */
  details: string;
}

/** The app's own first-run check (WebView2, service file, data folder, clock, port); `null` outside the app. */
export async function prereqCheck(): Promise<PrereqReport | null> {
  if (isMockMode() || !isTauriRuntime()) {
    return null;
  }
  try {
    return await invoke<PrereqReport>("prereq_check");
  } catch {
    return null;
  }
}

/** Open Microsoft's official WebView2 page in the browser (app only; the address is fixed in the app). */
export async function openWebview2Download(): Promise<void> {
  if (isTauriRuntime() && !isMockMode()) {
    await invoke("open_webview_download");
  }
}

/** Show the folder with `desktop-daemon.log` in the file manager (app only). */
export async function openLogFolder(): Promise<void> {
  if (isTauriRuntime() && !isMockMode()) {
    await invoke("open_log_folder");
  }
}
