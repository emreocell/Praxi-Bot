/**
 * How to add a settings card:
 * 1. Build a CardDef `{ id, section, title, description, keywords, kind, node }` in `views/settings/*Cards.tsx`.
 * 2. Register `{ page, advanced?, risk?, learn? }` in `CARD_META` below (missing entries fall back via `SECTION_PAGE`).
 * 3. Add any new i18n keys to `en.json`, `tr.json`, `de.json`, and `es.json` and run `pnpm --dir apps/desktop/ui sort:i18n`.
 * 4. Add the new card id to `FROZEN_CARD_IDS` in `lib/settingsPages.test.ts` so nothing silently vanishes.
 */
import type { AppSettings } from "../store/settingsStore.ts";
import { foldText } from "./searchText.ts";
import type { SettingsSectionId } from "../views/settings/types.ts";

/**
 * The settings, as a non-programmer sees them: at most 7 plainly named pages.
 * Cards keep their old technical `section` for fallback; `CARD_META` places each card here.
 */
export const SETTINGS_PAGES = [
  "general",
  "appearance",
  "models",
  "where",
  "apps",
  "safety",
  "system"
] as const;
export type SettingsPageId = (typeof SETTINGS_PAGES)[number];

/** Words that get a short plain-language "What is this?" popover (and a docs link). */
export type LearnTerm = "apiKey" | "permissions" | "folders" | "memory" | "provider" | "safeMode";

export interface CardMeta {
  page: SettingsPageId;
  /** Hidden behind "More options" until expanded (or when searching / targeted by a deep link). */
  advanced?: boolean;
  /** i18n key suffix of a one-line "what happens if you change this" warning. */
  risk?: string;
  learn?: LearnTerm;
}

/** Where every settings card lives. A card missing here falls back via its legacy `section`. */
export const CARD_META: Record<string, CardMeta> = {
  // General (You): 6 basic, 8 under "More options"
  account: { page: "general" },
  language: { page: "general" },
  send: { page: "general" },
  notifications: { page: "general" },
  "speak-replies": { page: "general" },
  dictation: { page: "general" },
  voice: { page: "general", advanced: true },
  "voice-input": { page: "general", advanced: true },
  times: { page: "general", advanced: true },
  autostart: { page: "general", advanced: true },
  keepInTray: { page: "general", advanced: true },
  quickAsk: { page: "general", advanced: true },
  hotkey: { page: "general", advanced: true },
  onboarding: { page: "general", advanced: true },

  // Appearance (You): 6 basic (corners promoted: owners look for it next to the theme), 6 under "More options"
  theme: { page: "appearance" },
  accent: { page: "appearance" },
  backdrop: { page: "appearance" },
  wallpaper: { page: "appearance" },
  corners: { page: "appearance" },
  accessibility: { page: "appearance" },
  style: { page: "appearance", advanced: true },
  clearChat: { page: "appearance", advanced: true },
  photorealAvatars: { page: "appearance", advanced: true },
  bubbles: { page: "appearance", advanced: true },
  background: { page: "appearance", advanced: true },
  frame: { page: "appearance", advanced: true },

  // AI models (Your bots): 5 basic, 3 under "More options"
  defaultModel: { page: "models", learn: "provider" },
  keys: { page: "models", learn: "apiKey" },
  detectedAccounts: { page: "models" },
  memory: { page: "models", learn: "memory", risk: "memory" },
  knowledge: { page: "models", learn: "memory" },
  localModels: { page: "models", advanced: true },
  helpers: { page: "models", advanced: true },
  providers: { page: "models", advanced: true, risk: "providers", learn: "provider" },

  // Computers (Your bots): 3 basic
  target: { page: "where" },
  "my-computer": { page: "where" },
  "bot-workspaces": { page: "where" },

  // Mail & web (Your bots): 2 basic
  "web-search": { page: "apps" },
  email: { page: "apps", risk: "email" },
  vault: { page: "apps" },

  // Safety (Your bots): 4 basic, 2 under "More options"
  "safety-levels": { page: "safety", learn: "permissions" },
  "page-reader": { page: "safety" },
  "trusted-folders": { page: "safety", risk: "folders", learn: "folders" },
  "allowed-grants": { page: "safety", learn: "permissions" },
  "site-rules": { page: "safety", advanced: true, learn: "permissions" },
  "audit-log": { page: "safety" },
  permissions: { page: "safety", advanced: true, risk: "permissions", learn: "permissions" },
  "safe-mode": { page: "safety", advanced: true, learn: "safeMode" },

  // System (App & data): 3 basic, 2 under "More options"
  usage: { page: "system" },
  data: { page: "system" },
  version: { page: "system" },
  diagnostics: { page: "system", advanced: true },
  logs: { page: "system", advanced: true }
};

const SECTION_PAGE: Record<SettingsSectionId, SettingsPageId> = {
  general: "general",
  appearance: "appearance",
  ai: "models",
  computer: "where",
  mail: "apps",
  usage: "system",
  diagnostics: "system",
  updates: "system"
};

/** Legacy section ids and retired page ids resolve to one of the 7 current pages. */
export const PAGE_ALIASES: Record<string, SettingsPageId> = {
  ...SECTION_PAGE,
  notifications: "general",
  voice: "general",
  models: "models",
  where: "where",
  apps: "apps",
  safety: "safety",
  data: "system",
  help: "system",
  advanced: "system"
};

export function pageOfSection(section: SettingsSectionId): SettingsPageId {
  return SECTION_PAGE[section];
}

/** Old `?section=` values, retired page ids and `initialSection` props still work. */
export function parsePage(raw: string | null | undefined): SettingsPageId {
  if (!raw) return "general";
  if ((SETTINGS_PAGES as readonly string[]).includes(raw)) return raw as SettingsPageId;
  if (raw in PAGE_ALIASES) return PAGE_ALIASES[raw];
  return "general";
}

export function metaOfCard(card: { id: string; section: SettingsSectionId }): CardMeta {
  const m = CARD_META[card.id];
  return m ? { ...m, page: parsePage(m.page) } : { page: pageOfSection(card.section) };
}

/** The pages in the left list, grouped; every page appears once. */
export const SETTINGS_PAGE_GROUPS: ReadonlyArray<{
  id: "you" | "bots" | "app";
  pages: readonly SettingsPageId[];
}> = [
  { id: "you", pages: ["general", "appearance"] },
  { id: "bots", pages: ["models", "where", "apps", "safety"] },
  { id: "app", pages: ["system"] }
];

// ---------------------------------------------------------------- simple / more options

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

/** A card shows when it is not advanced, or "More options" is open, or the user is searching for it. */
export function isCardShown(meta: CardMeta, mode: DetailMode, searching: boolean): boolean {
  return !meta.advanced || mode === "advanced" || searching;
}

// ---------------------------------------------------------------- reset to recommended

/** The app-held settings each page can reset; daemon-held state (permissions) resets separately. */
const RESET_KEYS: Partial<Record<SettingsPageId, readonly (keyof AppSettings)[]>> = {
  general: [
    "sendWith",
    "messageTimes",
    "autostart",
    "keepInTray",
    "quickAskEnabled",
    "quickAskHotkey",
    "stopHotkey",
    "notifyNeedsYou",
    "notifyLongRun",
    "notifyFailed",
    "notifyPrivate",
    "quietHours",
    "mutedBotIds",
    "digestTime",
    "digestLastDay",
    "voiceName",
    "voiceRate",
    "speakReplies",
    "pttEnabled",
    "pttKey",
    "micDeviceId",
    "silenceStopSec"
  ],
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
    "photorealAvatars",
    "textScale",
    "density",
    "dyslexiaFont",
    "reduceMotion",
    "reduceTransparency",
    "focusRing"
  ]
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
