import type { AppSettings } from "../store/settingsStore.ts";
import { foldText } from "./searchText.ts";
import type { SettingsSectionId } from "../views/settings/types.ts";

/**
 * The settings, as a non-programmer sees them: ten plainly named pages.  The cards keep their
 * old technical `section`; this module decides which page each card is shown on.
 */
export const SETTINGS_PAGES = [
  "general",
  "appearance",
  "models",
  "where",
  "apps",
  "safety",
  "notifications",
  "voice",
  "data",
  "advanced"
] as const;
export type SettingsPageId = (typeof SETTINGS_PAGES)[number];

/** Words that get a short plain-language "What is this?" popover (and a docs link). */
export type LearnTerm = "apiKey" | "permissions" | "folders" | "memory" | "provider" | "safeMode";

export interface CardMeta {
  page: SettingsPageId;
  /** Hidden until "Show advanced settings" is on (or the user searches for it). */
  advanced?: boolean;
  /** i18n key suffix of a one-line "what happens if you change this" warning. */
  risk?: string;
  learn?: LearnTerm;
}

/** Where every settings card lives.  A card missing here lands on "Advanced" (and a test fails). */
export const CARD_META: Record<string, CardMeta> = {
  account: { page: "general" },
  language: { page: "general" },
  send: { page: "general" },
  times: { page: "general" },
  autostart: { page: "general" },
  hotkey: { page: "general", advanced: true },
  onboarding: { page: "general" },

  // Appearance: theme, accent, backdrop and the wallpaper (with its Cloud motion control) stay in
  // the simple view; the finer shape and chrome options move behind "Advanced".
  style: { page: "appearance", advanced: true },
  corners: { page: "appearance", advanced: true },
  bubbles: { page: "appearance", advanced: true },
  clearChat: { page: "appearance", advanced: true },
  photorealAvatars: { page: "appearance", advanced: true },
  background: { page: "appearance", advanced: true },
  frame: { page: "appearance", advanced: true },

  detectedAccounts: { page: "models" },
  keys: { page: "models", learn: "apiKey" },
  defaultModel: { page: "models", learn: "provider" },
  localModels: { page: "models", advanced: true },
  providers: { page: "models", advanced: true, risk: "providers", learn: "provider" },

  target: { page: "where" },
  "my-computer": { page: "where" },
  "web-search": { page: "apps" },
  email: { page: "apps", risk: "email" },

  "safety-levels": { page: "safety", learn: "permissions" },
  permissions: { page: "safety", advanced: true, risk: "permissions", learn: "permissions" },
  "trusted-folders": { page: "safety", risk: "folders", learn: "folders" },
  "safe-mode": { page: "safety", learn: "safeMode" },

  notifications: { page: "notifications" },

  "speak-replies": { page: "voice" },
  voice: { page: "voice" },
  dictation: { page: "voice" },

  knowledge: { page: "data", learn: "memory" },
  memory: { page: "data", learn: "memory", risk: "memory" },
  usage: { page: "data" },

  diagnostics: { page: "advanced" },
  version: { page: "advanced" }
};

const SECTION_PAGE: Record<SettingsSectionId, SettingsPageId> = {
  general: "general",
  appearance: "appearance",
  ai: "models",
  computer: "where",
  mail: "apps",
  usage: "data",
  diagnostics: "advanced",
  updates: "advanced"
};

export function pageOfSection(section: SettingsSectionId): SettingsPageId {
  return SECTION_PAGE[section];
}

/** Old `?section=` values and `initialSection` props still work: they map to a page. */
export function parsePage(raw: string | null | undefined): SettingsPageId {
  if ((SETTINGS_PAGES as readonly string[]).includes(raw ?? "")) return raw as SettingsPageId;
  if (raw && raw in SECTION_PAGE) return SECTION_PAGE[raw as SettingsSectionId];
  return "general";
}

export function metaOfCard(card: { id: string; section: SettingsSectionId }): CardMeta {
  return CARD_META[card.id] ?? { page: pageOfSection(card.section) };
}

/** The pages in the left list, grouped; every page appears once. */
export const SETTINGS_PAGE_GROUPS: ReadonlyArray<{
  id: "you" | "bots" | "app";
  pages: readonly SettingsPageId[];
}> = [
  { id: "you", pages: ["general", "appearance", "notifications", "voice"] },
  { id: "bots", pages: ["models", "where", "apps", "safety"] },
  { id: "app", pages: ["data", "advanced"] }
];

// ---------------------------------------------------------------- simple / advanced

export type DetailMode = "simple" | "advanced";
export const DETAIL_MODE_KEY = "praxi.settings.detailMode";

export function parseDetailMode(raw: unknown): DetailMode {
  return raw === "advanced" ? "advanced" : "simple";
}

export function loadDetailMode(storage?: Pick<Storage, "getItem">): DetailMode {
  try {
    return parseDetailMode((storage ?? window.localStorage).getItem(DETAIL_MODE_KEY));
  } catch {
    return "simple";
  }
}

export function saveDetailMode(mode: DetailMode, storage?: Pick<Storage, "setItem">): void {
  try {
    (storage ?? window.localStorage).setItem(DETAIL_MODE_KEY, mode);
  } catch {
    // Private mode / blocked storage: the choice just is not remembered.
  }
}

/** A card shows when it is not advanced, or Advanced is on, or the user is searching for it. */
export function isCardShown(meta: CardMeta, mode: DetailMode, searching: boolean): boolean {
  return !meta.advanced || mode === "advanced" || searching || meta.page === "advanced";
}

// ---------------------------------------------------------------- reset to recommended

/** The app-held settings each page can reset; daemon-held state (permissions) resets separately. */
const RESET_KEYS: Partial<Record<SettingsPageId, readonly (keyof AppSettings)[]>> = {
  general: ["sendWith", "messageTimes", "autostart", "stopHotkey"],
  appearance: [
    "themeMode",
    "accentColor",
    "uiStyle",
    "corners",
    "bubble",
    "ownBubble",
    "backdrop",
    "wallpaper",
    "wallpaperMotion",
    "wallpaperVeil",
    "backdropDim",
    "panelTransparency",
    "panelBlur",
    "transparentChat",
    "chatPanel",
    "transparencyOverride",
    "photorealAvatars"
  ],
  notifications: ["notifyNeedsYou", "notifyLongRun", "notifyFailed", "notifyPrivate", "quietHours", "mutedBotIds"],
  voice: ["voiceName", "voiceRate", "speakReplies"]
};

/** The settings patch that restores a page to the recommended values (null = nothing to reset). */
export function resetPatch(page: SettingsPageId, defaults: AppSettings): Partial<AppSettings> | null {
  const keys = RESET_KEYS[page];
  if (!keys) return null;
  const patch: Record<string, unknown> = {};
  for (const k of keys) patch[k] = defaults[k];
  return patch as Partial<AppSettings>;
}

/** Safety has its own reset: the workspace preset goes back to the recommended level. */
export const RECOMMENDED_SAFETY_PRESET = "approve-destructive";

export function canReset(page: SettingsPageId): boolean {
  return page === "safety" || RESET_KEYS[page] !== undefined;
}

// ---------------------------------------------------------------- safety levels

export interface SafetyLevel {
  id: "careful" | "balanced" | "handsOff";
  /** The daemon preset this level stands for. */
  preset: "read-and-draft" | "approve-destructive" | "full";
  recommended?: boolean;
}

export const SAFETY_LEVELS: readonly SafetyLevel[] = [
  { id: "careful", preset: "read-and-draft" },
  { id: "balanced", preset: "approve-destructive", recommended: true },
  { id: "handsOff", preset: "full" }
];

export function levelOfPreset(preset: string | null | undefined): SafetyLevel["id"] | null {
  return SAFETY_LEVELS.find((l) => l.preset === preset)?.id ?? null;
}

// ---------------------------------------------------------------- search highlighting

/** The folded words of a search query. */
export function queryWords(query: string): string[] {
  return foldText(query).split(/\s+/).filter(Boolean);
}

/**
 * Split `text` into plain and matching parts for `<mark>`.  Matching is diacritic-insensitive
 * (like the search itself): "gonder" marks "Gönder".  Lengths stay equal under folding for the
 * characters the search folds, so offsets carry over.
 */
export function highlightParts(text: string, query: string): Array<{ text: string; hit: boolean }> {
  const words = queryWords(query);
  if (words.length === 0 || text === "") return [{ text, hit: false }];
  const folded = foldText(text);
  if (folded.length !== text.length) return [{ text, hit: false }];
  const hit = new Array<boolean>(text.length).fill(false);
  for (const w of words) {
    for (let at = folded.indexOf(w); at !== -1; at = folded.indexOf(w, at + 1)) {
      for (let i = at; i < at + w.length; i++) hit[i] = true;
    }
  }
  const out: Array<{ text: string; hit: boolean }> = [];
  let start = 0;
  for (let i = 1; i <= text.length; i++) {
    if (i === text.length || hit[i] !== hit[start]) {
      out.push({ text: text.slice(start, i), hit: hit[start] });
      start = i;
    }
  }
  return out;
}
