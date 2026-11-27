import { invoke } from "@tauri-apps/api/core";
import { isMockMode } from "../api/mockMode";
import { isTauriRuntime } from "../overlay/events";
export { updateErrorKey } from "./updateErrors";

/** What the desktop shell says about updates (see apps/desktop/src/update.rs). */
export interface UpdateStatus {
  enabled: boolean;
  currentVersion: string;
  buildId: string;
  channel: UpdateChannel;
  previewAvailable: boolean;
  lastChecked: number | null;
  skippedVersion: string | null;
  installedNotes: UpdateInfo | null;
  previousVersion: string | null;
  rollbackAdvised: boolean;
  rollbackAvailable: boolean;
}

export type UpdateChannel = "stable" | "preview";

export interface UpdateInfo {
  version: string;
  /** Plain text in the user's language when the release has it; never HTML. */
  notes: string;
  date: string | null;
}

const OFF: UpdateStatus = {
  enabled: false,
  currentVersion: "",
  buildId: "",
  channel: "stable",
  previewAvailable: false,
  lastChecked: null,
  skippedVersion: null,
  installedNotes: null,
  previousVersion: null,
  rollbackAdvised: false,
  rollbackAvailable: false
};

const inApp = () => isTauriRuntime() && !isMockMode();

/**
 * Design-review states of the Updates page (mock mode only): `?update=available` (a check finds a
 * newer version), `?update=current` (a check finds nothing). Without it mock mode shows the honest
 * "not set up" state a build without a signed update feed has.
 */
function mockUpdates(): "available" | "current" | null {
  if (!isMockMode() || typeof window === "undefined") return null;
  const v = new URLSearchParams(window.location.search).get("update");
  return v === "available" || v === "current" ? v : null;
}

const MOCK_STATUS: UpdateStatus = {
  ...OFF,
  enabled: true,
  currentVersion: "0.1.0-alpha.8",
  buildId: "72f415a3",
  previewAvailable: true,
  lastChecked: Date.parse("2026-11-26T18:20:00Z") / 1000
};

export async function updateStatus(lang = "en"): Promise<UpdateStatus> {
  if (mockUpdates()) {
    return MOCK_STATUS;
  }
  if (!inApp()) {
    return OFF;
  }
  return invoke<UpdateStatus>("update_status", { lang });
}

export async function setUpdateChannel(channel: UpdateChannel): Promise<void> {
  if (mockUpdates()) return;
  return invoke("update_set_channel", { channel });
}

export async function skipUpdate(): Promise<void> {
  if (mockUpdates()) return;
  return invoke("update_skip");
}

/** `lang` is the UI language ("tr", "de", "en"); the shell picks the matching notes. */
export async function checkForUpdate(lang: string): Promise<UpdateInfo | null> {
  const mock = mockUpdates();
  if (mock) {
    return mock === "available"
      ? {
          version: "0.1.0-alpha.9",
          date: "2026-11-26T12:00:00Z",
          notes: lang === "tr"
            ? "• Ayarlar yeniden düzenlendi: Kasa ve Güncellemeler kendi sayfalarında.\n• Kodlama CLI'larının kurulum ve giriş adımları tek satırda.\n• Hata düzeltmeleri."
            : "• Settings reorganised: Vault and Updates have their own pages.\n• Coding CLI install and sign-in steps live in each CLI's row.\n• Bug fixes."
        }
      : null;
  }
  return invoke<UpdateInfo | null>("update_check", { lang });
}

/** Downloads, verifies the signature and installs; the app restarts on Windows. */
export async function installUpdate(): Promise<void> {
  if (mockUpdates()) return;
  return invoke("update_install");
}

export async function rollbackUpdate(): Promise<void> {
  if (mockUpdates()) return;
  return invoke("update_rollback");
}
