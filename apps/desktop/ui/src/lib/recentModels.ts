import type { ModelGroup, ModelOption } from "./modelPicker.ts";

/**
 * The models picked last in the composer, newest first, remembered on this device only
 * (localStorage). The model picker lists them on top, so switching back is one click.
 */
export const RECENT_MODELS_MAX = 3;
const KEY = "praxi.recentModels.v1";

/** `ref` moved to the front, without duplicates, at most `max` long. */
export function withRecent(list: readonly string[], ref: string, max = RECENT_MODELS_MAX): string[] {
  const clean = ref.trim();
  if (!clean) return [...list].slice(0, max);
  return [clean, ...list.filter((r) => r !== clean)].slice(0, max);
}

/** Stored value -> list of refs; anything unexpected reads as "no recent models". */
export function parseRecentModels(raw: unknown): string[] {
  if (typeof raw !== "string") return [];
  try {
    const value: unknown = JSON.parse(raw);
    return Array.isArray(value)
      ? value.filter((r): r is string => typeof r === "string" && r.includes("/")).slice(0, RECENT_MODELS_MAX)
      : [];
  } catch {
    return [];
  }
}

/** The recent refs that can still be picked here (a provider switched off drops out), in recent order. */
export function recentOptions(groups: readonly ModelGroup[], refs: readonly string[]): ModelOption[] {
  const out: ModelOption[] = [];
  for (const ref of refs) {
    const option = groups.flatMap((g) => g.models).find((m) => m.ref === ref);
    if (option) out.push(option);
  }
  return out;
}

export function loadRecentModels(): string[] {
  try {
    return parseRecentModels(window.localStorage?.getItem(KEY));
  } catch {
    return [];
  }
}

export function saveRecentModels(list: readonly string[]): void {
  try {
    window.localStorage?.setItem(KEY, JSON.stringify(list));
  } catch {
    // Not remembered when storage is unavailable; the picker still works.
  }
}
