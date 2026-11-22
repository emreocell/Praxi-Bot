import { invoke } from "@tauri-apps/api/core";
import { isMockMode } from "../api/mockMode";
import { isTauriRuntime } from "../overlay/events";
export { updateErrorKey } from "./updateErrors";

/** What the desktop shell says about updates (see apps/desktop/src/update.rs). */
export interface UpdateStatus {
  enabled: boolean;
  currentVersion: string;
  previousVersion: string | null;
  rollbackAdvised: boolean;
  rollbackAvailable: boolean;
}

export interface UpdateInfo {
  version: string;
  /** Plain text in the user's language when the release has it; never HTML. */
  notes: string;
  date: string | null;
}

const OFF: UpdateStatus = {
  enabled: false,
  currentVersion: "",
  previousVersion: null,
  rollbackAdvised: false,
  rollbackAvailable: false
};

const inApp = () => isTauriRuntime() && !isMockMode();

export async function updateStatus(): Promise<UpdateStatus> {
  if (!inApp()) {
    return OFF;
  }
  try {
    return await invoke<UpdateStatus>("update_status");
  } catch {
    return OFF;
  }
}

/** `lang` is the UI language ("tr", "de", "en"); the shell picks the matching notes. */
export function checkForUpdate(lang: string): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>("update_check", { lang });
}

/** Downloads, verifies the signature and installs; the app restarts on Windows. */
export function installUpdate(): Promise<void> {
  return invoke("update_install");
}

export function rollbackUpdate(): Promise<void> {
  return invoke("update_rollback");
}
