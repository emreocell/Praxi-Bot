import { isQualityMode, type QualityMode } from "./liveViewAdaptive.ts";

const KEY = "praxi.liveView.quality";

/** The owner's saved picture-quality choice; a per-viewer convenience, so any storage failure is `auto`. */
export function loadQualityMode(storage: Pick<Storage, "getItem"> | null = safeStorage()): QualityMode {
  try {
    const v = storage?.getItem(KEY);
    return isQualityMode(v) ? v : "auto";
  } catch {
    return "auto";
  }
}

export function saveQualityMode(mode: QualityMode, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(KEY, mode);
  } catch {
    // Private window or blocked storage: the choice just lasts for this session.
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
