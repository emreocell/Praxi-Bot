/**
 * How to add a settings card:
 * 1. Build a CardDef `{ id, section, title, description, keywords, kind, node }` in `views/settings/*Cards.tsx`.
 * 2. Register `{ page, group, advanced?, risk?, learn? }` in `CARD_META` below (missing entries fall back via `SECTION_PAGE`).
 * 3. Add any new i18n keys to `en.json`, `tr.json`, `de.json`, and `es.json` and run `pnpm --dir apps/desktop/ui sort:i18n`.
 * 4. Add the new card id to `FROZEN_CARD_IDS` in `lib/settingsPages.test.ts` so nothing silently vanishes.
 */
import type { AppSettings } from "../store/settingsStore.ts";
import { foldText } from "./searchText.ts";
import type { SettingsSectionId } from "../views/settings/types.ts";

/**
 * The settings, as a non-programmer sees them: six plainly named pages, each split into a few
 * titled groups (`SETTINGS_GROUPS`).  Cards keep their old technical `section` for fallback;
 * `CARD_META` places each card on a page and in a group.
 */
export const SETTINGS_PAGES = [
  "general",
  "appearance",
  "models",
  "connections",
  "safety",
  "system"
] as const;
export type SettingsPageId = (typeof SETTINGS_PAGES)[number];

/**
 * The titled groups of each page, in order.  Rarely used cards of a group sit under the page's one
 * collapsed "Advanced settings" disclosure, under the same group title.
 */
export const SETTINGS_GROUPS = {
  general: ["you", "notifications", "voice", "startup"],
  appearance: ["look", "readability"],
  models: ["models", "memory"],
  connections: ["computers", "mailWeb", "vault"],
  safety: ["level", "access", "history"],
  system: ["usage", "about"]
} as const satisfies Record<SettingsPageId, readonly string[]>;
export type SettingsGroupId = (typeof SETTINGS_GROUPS)[SettingsPageId][number];

/** Words that get a short plain-language "What is this?" popover (and a docs link). */
export type LearnTerm = "apiKey" | "permissions" | "folders" | "memory" | "provider" | "safeMode";

export interface CardMeta {
  page: SettingsPageId;
  /** The titled group on the page (one of `SETTINGS_GROUPS[page]`). */
  group?: SettingsGroupId;
  /** Hidden behind "More options" until expanded (or when searching / targeted by a deep link). */
  advanced?: boolean;
  /** i18n key suffix of a one-line "what happens if you change this" warning. */
  risk?: string;
  learn?: LearnTerm;
}

/** Where every settings card lives. A card missing here falls back via its legacy `section`. */
export const CARD_META: Record<string, CardMeta> = {
  // General: you, notifications, voice, start-up (rarely used ones under Advanced)
  account: { page: "general", group: "you" },
  language: { page: "general", group: "you" },
  send: { page: "general", group: "you" },
  times: { page: "general", group: "you", advanced: true },
  notifications: { page: "general", group: "notifications" },
  "speak-replies": { page: "general", group: "voice" },
  dictation: { page: "general", group: "voice" },
  voice: { page: "general", group: "voice", advanced: true },
  "voice-input": { page: "general", group: "voice", advanced: true },
  autostart: { page: "general", group: "startup", advanced: true },
  keepInTray: { page: "general", group: "startup", advanced: true },
  quickAsk: { page: "general", group: "startup", advanced: true },
  hotkey: { page: "general", group: "startup", advanced: true },
  onboarding: { page: "general", group: "startup", advanced: true },

  // Appearance: the look, and readability (corners is basic: owners look for it next to the theme, #889)
  theme: { page: "appearance", group: "look" },
  accent: { page: "appearance", group: "look" },
  backdrop: { page: "appearance", group: "look" },
  wallpaper: { page: "appearance", group: "look" },
  accessibility: { page: "appearance", group: "readability" },
  style: { page: "appearance", group: "look", advanced: true },
  clearChat: { page: "appearance", group: "look", advanced: true },
  photorealAvatars: { page: "appearance", group: "look", advanced: true },
  corners: { page: "appearance", group: "look" },
  bubbles: { page: "appearance", group: "look", advanced: true },
  background: { page: "appearance", group: "look", advanced: true },
  frame: { page: "appearance", group: "look", advanced: true },

  // AI: which models the bots think with, and what they remember
  defaultModel: { page: "models", group: "models", learn: "provider" },
  keys: { page: "models", group: "models", learn: "apiKey" },
  detectedAccounts: { page: "models", group: "models" },
  localModels: { page: "models", group: "models", advanced: true },
  helpers: { page: "models", group: "models", advanced: true },
  providers: { page: "models", group: "models", advanced: true, risk: "providers", learn: "provider" },
  memory: { page: "models", group: "memory", learn: "memory", risk: "memory" },
  knowledge: { page: "models", group: "memory", learn: "memory" },

  // Connections: where the bots work, and what they connect to (mail, web, passwords)
  target: { page: "connections", group: "computers" },
  "my-computer": { page: "connections", group: "computers" },
  "bot-workspaces": { page: "connections", group: "computers", advanced: true },
  "web-search": { page: "connections", group: "mailWeb" },
  email: { page: "connections", group: "mailWeb" },
  "email-channel": { page: "connections", group: "mailWeb", advanced: true },
  "mail-guard": { page: "connections", group: "mailWeb", advanced: true },
  vault: { page: "connections", group: "vault" },

  // Safety: how freely bots work, what they may reach, what they did
  "safety-levels": { page: "safety", group: "level", learn: "permissions" },
  permissions: { page: "safety", group: "level", advanced: true, risk: "permissions", learn: "permissions" },
  "safe-mode": { page: "safety", group: "level", advanced: true, learn: "safeMode" },
  "trusted-folders": { page: "safety", group: "access", risk: "folders", learn: "folders" },
  "allowed-grants": { page: "safety", group: "access", learn: "permissions" },
  "page-reader": { page: "safety", group: "access" },
  "site-rules": { page: "safety", group: "access", advanced: true, learn: "permissions" },
  "audit-log": { page: "safety", group: "history" },

  // System: usage and storage, version and troubleshooting
  usage: { page: "system", group: "usage" },
  data: { page: "system", group: "usage" },
  version: { page: "system", group: "about" },
  diagnostics: { page: "system", group: "about", advanced: true },
  logs: { page: "system", group: "about", advanced: true }
};

const SECTION_PAGE: Record<SettingsSectionId, SettingsPageId> = {
  general: "general",
  appearance: "appearance",
  ai: "models",
  computer: "connections",
  mail: "connections",
  usage: "system",
  diagnostics: "system",
  updates: "system"
};

/** Legacy section ids and retired page ids (Computers and Mail & web are now Connections) resolve to a current page. */
export const PAGE_ALIASES: Record<string, SettingsPageId> = {
  ...SECTION_PAGE,
  notifications: "general",
  voice: "general",
  models: "models",
  where: "connections",
  apps: "connections",
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
  { id: "bots", pages: ["models", "connections", "safety"] },
  { id: "app", pages: ["system"] }
];

// ---------------------------------------------------------------- simple / more options

export type DetailMode = "simple" | "advanced";

/**
 * "Advanced settings" starts closed every time Settings opens, so the pages stay short; it opens
 * by itself only for a deep link or a search result that points at an advanced card.
 */
export function initialDetailMode(targetIsAdvanced: boolean): DetailMode {
  return targetIsAdvanced ? "advanced" : "simple";
}

/** The group a card is drawn under: its own, else the page's first group. */
export function groupOfCard(meta: CardMeta): SettingsGroupId {
  const groups: readonly SettingsGroupId[] = SETTINGS_GROUPS[meta.page];
  return meta.group && groups.includes(meta.group) ? meta.group : (groups[0] as SettingsGroupId);
}

/** Cards in page order, split into their titled groups (empty groups are left out). */
export function groupCards<T>(page: SettingsPageId, cards: readonly T[], meta: (card: T) => CardMeta): Array<{ group: SettingsGroupId; cards: T[] }> {
  const groups: readonly SettingsGroupId[] = SETTINGS_GROUPS[page];
  return groups
    .map((group) => ({ group, cards: cards.filter((c) => groupOfCard(meta(c)) === group) }))
    .filter((g) => g.cards.length > 0);
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

/**
 * Settings search: every word of the query must appear in the card's words (title, description,
 * keywords, page and group names).  Turkish and German add endings to words ("anahtarı",
 * "başlangıçta"), so a long query word also matches when the text has its stem (the word without
 * its last one or two letters, at least five letters left, so short words are never cut).
 */
export function settingsSearchMatch(query: string, ...texts: Array<string | null | undefined>): boolean {
  const words = queryWords(query);
  if (words.length === 0) return true;
  const hay = foldText(texts.filter(Boolean).join(" "));
  return words.every((w) => {
    if (hay.includes(w)) return true;
    for (let cut = 1; cut <= 2; cut++) {
      const stem = w.slice(0, w.length - cut);
      if (stem.length >= 5 && hay.includes(stem)) return true;
    }
    return false;
  });
}

/** Lower is better: query words missing from the card's title weigh most, then those missing from its description. */
export function settingsSearchRank(query: string, title: string, description: string): number {
  const words = queryWords(query);
  const missing = (text: string) => words.filter((w) => !settingsSearchMatch(w, text)).length;
  return missing(title) * 10 + missing(description);
}

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
