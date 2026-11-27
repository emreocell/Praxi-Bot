import type { ScreenFit } from "./liveViewInput.ts";

const KEY = "praxi.liveView.screenFit";

/**
 * How a screen share sits in its box, by default: `contain` shows the whole screen with bars;
 * `cover` fills the box and crops the edges (the opt-in toggle). A per-viewer convenience, so
 * any storage failure falls back to the default.
 */
export const DEFAULT_SCREEN_FIT: ScreenFit = "contain";

export function isScreenFit(value: unknown): value is ScreenFit {
  return value === "cover" || value === "contain";
}

export function loadScreenFit(storage: Pick<Storage, "getItem"> | null = safeStorage()): ScreenFit {
  try {
    const v = storage?.getItem(KEY);
    return isScreenFit(v) ? v : DEFAULT_SCREEN_FIT;
  } catch {
    return DEFAULT_SCREEN_FIT;
  }
}

export function saveScreenFit(fit: ScreenFit, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(KEY, fit);
  } catch {
    // Private window or blocked storage: the choice just lasts for this session.
  }
}

export function toggleScreenFit(fit: ScreenFit): ScreenFit {
  return fit === "cover" ? "contain" : "cover";
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** One shared choice for the bot panel's preview and the big viewer, so they never disagree. */
let current: ScreenFit | null = null;
const listeners = new Set<() => void>();

export function getScreenFit(): ScreenFit {
  current ??= loadScreenFit();
  return current;
}

export function setScreenFit(fit: ScreenFit): void {
  current = fit;
  saveScreenFit(fit);
  for (const l of listeners) {
    l();
  }
}

export function subscribeScreenFit(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
