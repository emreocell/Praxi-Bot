import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { AppSettings } from "../store/settingsStore.ts";
import { SETTINGS_SECTIONS, type SettingsSectionId } from "../views/settings/types.ts";
import { matchesQuery } from "./searchText.ts";
import { VOICE_KEY_TARGET } from "./settingsLink.ts";
import {
  CARD_META,
  canReset,
  highlightParts,
  isCardShown,
  levelOfPreset,
  metaOfCard,
  PAGE_ALIASES,
  parsePage,
  RECOMMENDED_SAFETY_PRESET,
  resetPatch,
  SAFETY_LEVELS,
  SETTINGS_PAGE_GROUPS,
  SETTINGS_GROUPS,
  SETTINGS_PAGES,
  settingsSearchMatch,
  settingsSearchRank,
  groupCards,
  groupOfCard,
  initialDetailMode,
  pageOfSection
} from "./settingsPages.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

/** Frozen inventory of every existing card id so nothing silently vanishes in future edits. */
const FROZEN_CARD_IDS = [
  "account",
  "language",
  "send",
  "times",
  "speak-replies",
  "voice",
  "voice-input",
  "dictation",
  "autostart",
  "keepInTray",
  "quickAsk",
  "hotkey",
  "notifications",
  "onboarding",
  "theme",
  "accent",
  "style",
  "clearChat",
  "photorealAvatars",
  "backdrop",
  "wallpaper",
  "corners",
  "bubbles",
  "background",
  "accessibility",
  "frame",
  "defaultModel",
  "keys",
  "detectedAccounts",
  "memory",
  "knowledge",
  "localModels",
  "helpers",
  "providers",
  "safety-levels",
  "audit-log",
  "target",
  "my-computer",
  "trusted-folders",
  "allowed-grants",
  "site-rules",
  "permissions",
  "web-search",
  "email",
  "logs",
  "usage",
  "diagnostics",
  "data",
  "safe-mode",
  "version"
] as const;

test("at most 7 pages and every page is in exactly one nav group", () => {
  assert.ok(SETTINGS_PAGES.length <= 7, `expected <= 7 pages, got ${SETTINGS_PAGES.length}`);
  const grouped = SETTINGS_PAGE_GROUPS.flatMap((g) => [...g.pages]);
  assert.equal(new Set(grouped).size, grouped.length);
  assert.deepEqual([...grouped].sort(), [...SETTINGS_PAGES].sort());
});

test("old section ids, retired page ids, and SettingsTargets all resolve to a real page", () => {
  const legacyAndRetired = [
    ...SETTINGS_SECTIONS,
    "ai",
    "computer",
    "mail",
    "usage",
    "diagnostics",
    "updates",
    "models",
    "where",
    "apps",
    "safety",
    "data",
    "help",
    "advanced",
    "notifications",
    "voice"
  ];
  for (const s of SETTINGS_SECTIONS) {
    assert.ok(SETTINGS_PAGES.includes(pageOfSection(s)));
  }
  for (const id of legacyAndRetired) {
    assert.ok(SETTINGS_PAGES.includes(parsePage(id)), `${id} -> ${parsePage(id)}`);
    assert.ok(id in PAGE_ALIASES || (SETTINGS_PAGES as readonly string[]).includes(id), id);
  }
  assert.equal(parsePage("ai"), "models");
  assert.equal(parsePage("computer"), "connections");
  assert.equal(parsePage("mail"), "connections");
  assert.equal(parsePage("notifications"), "general");
  assert.equal(parsePage("voice"), "general");
  assert.equal(parsePage("data"), "system");
  assert.equal(parsePage("help"), "system");
  assert.equal(parsePage("advanced"), "system");
  assert.equal(parsePage("safety"), "safety");
  assert.equal(parsePage("nonsense"), "general");
  assert.equal(parsePage(null), "general");

  // Every SettingsTarget used in code resolves to a real page and card.
  const targets = [
    VOICE_KEY_TARGET,
    { page: "models", card: "keys", field: "api-key" }
  ];
  for (const target of targets) {
    assert.ok(SETTINGS_PAGES.includes(parsePage(target.page)), target.page);
    assert.ok(target.card in CARD_META, target.card);
  }
});

test("every card in the frozen list and every CardDef is placed on a real page, and unknown cards fall back via section", () => {
  for (const id of FROZEN_CARD_IDS) {
    assert.ok(id in CARD_META, `missing CARD_META for ${id}`);
    assert.ok(SETTINGS_PAGES.includes(CARD_META[id].page), `${id} -> ${CARD_META[id].page}`);
  }
  for (const [id, meta] of Object.entries(CARD_META)) {
    assert.ok(SETTINGS_PAGES.includes(meta.page), `${id} -> ${meta.page}`);
  }
  // An unplaced card from another branch falls back cleanly via its legacy section.
  for (const section of SETTINGS_SECTIONS) {
    const fallback = metaOfCard({ id: "future-card", section: section as SettingsSectionId });
    assert.ok(SETTINGS_PAGES.includes(fallback.page), section);
  }
});

test("Basic hides advanced cards and search reveals them", () => {
  const adv = { page: "models", advanced: true } as const;
  assert.equal(isCardShown(adv, "simple", false), false);
  assert.equal(isCardShown(adv, "advanced", false), true);
  assert.equal(isCardShown(adv, "simple", true), true);
  assert.equal(isCardShown({ page: "models" }, "simple", false), true);
});

test("Advanced settings starts closed on every open; only a deep link to an advanced card opens it", () => {
  assert.equal(initialDetailMode(false), "simple");
  assert.equal(initialDetailMode(true), "advanced");
});

test("six pages, each split into titled groups; every card sits in a group of its own page", () => {
  assert.deepEqual([...SETTINGS_PAGES], ["general", "appearance", "models", "connections", "safety", "system"]);
  for (const page of SETTINGS_PAGES) assert.ok(SETTINGS_GROUPS[page].length >= 2, page);
  for (const [id, meta] of Object.entries(CARD_META)) {
    assert.ok(meta.group, `${id} has a group`);
    assert.ok((SETTINGS_GROUPS[meta.page] as readonly string[]).includes(meta.group as string), `${id}: ${meta.group} on ${meta.page}`);
  }
  // Computers and Mail & web are one page now; their old ids and sections still land there.
  for (const old of ["where", "apps", "computer", "mail"]) assert.equal(parsePage(old), "connections", old);
  // Every page keeps a short basic view: rarely used cards are under Advanced.
  for (const page of SETTINGS_PAGES) {
    const basic = Object.values(CARD_META).filter((m) => m.page === page && !m.advanced).length;
    assert.ok(basic >= 1 && basic <= 8, `${page}: ${basic} basic cards`);
  }
});

test("cards are drawn group by group in the page's order; a card without a group joins the first", () => {
  const cards = [
    { id: "a", meta: { page: "general", group: "voice" } },
    { id: "b", meta: { page: "general", group: "you" } },
    { id: "c", meta: { page: "general" } },
    { id: "d", meta: { page: "general", group: "voice" } }
  ] as const;
  const out = groupCards("general", cards, (c) => c.meta);
  assert.deepEqual(out.map((g) => [g.group, g.cards.map((c) => c.id)]), [["you", ["b", "c"]], ["voice", ["a", "d"]]]);
  assert.equal(groupOfCard({ page: "system", group: "voice" }), "usage");
});

test("search forgives Turkish and German word endings, accents and case, but still needs every word", () => {
  assert.ok(settingsSearchMatch("api anahtarı", "API keys", "API anahtarları ve hesaplar"));
  assert.ok(settingsSearchMatch("başlangıçta", "Windows başlangıcında aç", "baslangic otomatik"));
  assert.ok(settingsSearchMatch("Bildirimleri", "Bildirimler"));
  assert.ok(settingsSearchMatch("SCHLÜSSELN", "Schlüssel"));
  assert.ok(!settingsSearchMatch("api xyzzy", "API keys"));
  // Short words are never cut: "dil" (language) must not match every "di…" word.
  assert.ok(!settingsSearchMatch("dile", "dikte"));
  assert.ok(!settingsSearchMatch("ollama", "kollar"));
  assert.ok(settingsSearchMatch("", "anything"));
  // The card named after the words comes first.
  const keys = settingsSearchRank("api anahtarı", "Erişim anahtarları", "Anahtarlar sistem anahtarlığında saklanır");
  const dictation = settingsSearchRank("api anahtarı", "Sesli dikte", "OpenAI uyumlu bir servis, API anahtarı");
  assert.ok(keys < dictation, `${keys} < ${dictation}`);
  assert.equal(settingsSearchRank("tema", "Tema", "Renkler"), 1);
  assert.equal(settingsSearchRank("tema", "Tema", "Tema rengi"), 0);
});

test("the three safety levels map to daemon presets, balanced is the recommended default", () => {
  assert.deepEqual(
    SAFETY_LEVELS.map((l) => l.preset),
    ["read-and-draft", "approve-destructive", "full"]
  );
  assert.equal(SAFETY_LEVELS.filter((l) => l.recommended).length, 1);
  assert.equal(SAFETY_LEVELS.find((l) => l.recommended)?.preset, RECOMMENDED_SAFETY_PRESET);
  assert.equal(levelOfPreset("approve-destructive"), "balanced");
  assert.equal(levelOfPreset("read-only"), null);
  assert.equal(levelOfPreset(null), null);
});

const DEFAULTS = {
  speakReplies: false,
  voiceName: "",
  voiceRate: 1,
  wallpaperMotion: "cloud",
  language: "en",
  sendWith: "enter",
  notifyNeedsYou: true
} as unknown as AppSettings;

test("reset to recommended restores defaults only for the page's own settings", () => {
  const general = resetPatch("general", DEFAULTS);
  assert.equal(general?.speakReplies, false);
  assert.equal(general?.sendWith, "enter");
  assert.equal(general?.notifyNeedsYou, true);
  assert.ok(!("language" in (general ?? {})), "language is not reset");
  const appearance = resetPatch("appearance", DEFAULTS);
  assert.ok(appearance && "wallpaperMotion" in appearance);
  assert.equal(resetPatch("models", DEFAULTS), null);
  assert.equal(canReset("safety"), true);
  assert.equal(canReset("system"), false);
});

test("search highlighting ignores accents and case and merges overlapping words", () => {
  assert.deepEqual(highlightParts("Gönder with Enter", "gonder"), [
    { text: "Gönder", hit: true },
    { text: " with Enter", hit: false }
  ]);
  assert.deepEqual(highlightParts("Send now", "se end"), [
    { text: "Send", hit: true },
    { text: " now", hit: false }
  ]);
  assert.deepEqual(highlightParts("Nothing", ""), [{ text: "Nothing", hit: false }]);
});

test("search finds every card by its English title and by en, tr and de synonyms", () => {
  const en = JSON.parse(read("../i18n/locales/en.json")) as Record<string, unknown>;
  const getKey = (key: string): string => {
    const val = key.split(".").reduce<unknown>((acc, part) => (acc as Record<string, unknown>)?.[part], en);
    assert.equal(typeof val, "string", `missing en.json key ${key}`);
    return val as string;
  };

  const sources = [
    read("../views/settings/GeneralCards.tsx"),
    read("../views/settings/AppearanceCards.tsx"),
    read("../views/settings/AiCards.tsx"),
    read("../views/settings/ComputerCards.tsx"),
    read("../views/settings/MailCards.tsx"),
    read("../views/settings/OtherCards.tsx")
  ].join("\n");

  const extractKeywords = (id: string): string => {
    const escaped = id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const callForm = new RegExp(`card\\(\\s*"${escaped}"[\\s\\S]*?keywords:\\s*"([^"]+)"`);
    const objForm = new RegExp(`id:\\s*"${escaped}"[\\s\\S]*?keywords:\\s*"([^"]+)"`);
    return (callForm.exec(sources) ?? objForm.exec(sources))?.[1] ?? "";
  };

  const cases: Array<[id: string, titleKey: string, descKey: string, enSyn: string, trSyn: string, deSyn: string]> = [
    ["account", "settingsModal.general.account.title", "settingsModal.general.account.description", "account", "isim", "name"],
    ["language", "settings.language.title", "settings.language.description", "language", "dil", "sprache"],
    ["send", "settingsModal.general.send.title", "settingsModal.general.send.description", "send", "gonder", "senden"],
    ["times", "settingsModal.general.times.title", "settingsModal.general.times.description", "clock", "saat", "uhrzeit"],
    ["speak-replies", "voice.speakAuto.title", "voice.speakAuto.description", "speak", "sesli", "vorlesen"],
    ["voice", "voice.title", "voice.description", "voice", "ses", "stimme"],
    ["voice-input", "voice.input.title", "voice.input.description", "microphone", "mikrofon", "sprechtaste"],
    ["dictation", "voice.stt.title", "voice.stt.description", "dictation", "dikte", "diktat"],
    ["autostart", "settings.autostart.title", "settings.autostart.description", "startup", "baslangic", "anmeldung"],
    ["keepInTray", "settings.keepInTray.title", "settings.keepInTray.description", "tray", "tepsi", "hintergrund"],
    ["quickAsk", "settings.quickAsk.title", "settings.quickAsk.description", "quick ask", "hizli sor", "schnell fragen"],
    ["hotkey", "settings.hotkey.title", "settings.hotkey.description", "shortcut", "kisayol", "tastenkombination"],
    ["notifications", "settings.notifications.title", "settings.notifications.description", "notifications", "bildirim", "benachrichtigung"],
    ["onboarding", "settings.onboarding.title", "settings.onboarding.description", "wizard", "kurulum", "einrichtung"],
    ["theme", "appearance.theme.title", "appearance.theme.description", "theme", "tema", "dunkel"],
    ["accent", "appearance.accent.title", "appearance.accent.description", "color", "renk", "akzent"],
    ["style", "appearance.style.title", "appearance.style.description", "glass", "cam", "transparenz"],
    ["clearChat", "appearance.clearChat.title", "appearance.clearChat.description", "transparent", "saydam", "transparent"],
    ["photorealAvatars", "appearance.photorealAvatars.title", "appearance.photorealAvatars.description", "avatar", "kil", "fotorealistisch"],
    ["backdrop", "appearance.backdrop.title", "appearance.backdrop.description", "backdrop", "arka plan", "backdrop"],
    ["wallpaper", "appearance.wallpaper.title", "appearance.wallpaper.description", "wallpaper", "duvar kagidi", "wolken"],
    ["corners", "appearance.corners.title", "appearance.corners.description", "radius", "koseler", "corners"],
    ["bubbles", "appearance.bubbles.title", "appearance.bubbles.description", "bubble", "balon", "bubble"],
    ["background", "settings.background.title", "settings.background.description", "wallpaper", "arka plan", "image"],
    ["accessibility", "appearance.accessibility.title", "appearance.accessibility.description", "accessibility", "erisilebilirlik", "barrierefreiheit"],
    ["frame", "settingsModal.general.frame.title", "settingsModal.general.frame.description", "window", "pencere", "frame"],
    ["defaultModel", "settingsModal.ai.defaultModel.title", "settingsModal.ai.defaultModel.description", "model", "varsayilan", "standartmodell"],
    ["keys", "settingsModal.ai.keys.title", "settingsModal.ai.keys.description", "password", "anahtar", "schlüssel"],
    ["detectedAccounts", "settingsModal.ai.detected.title", "settingsModal.ai.detected.description", "cli", "algilanan", "werkzeuge"],
    ["memory", "memory.title", "memory.description", "forget", "hafiza", "löschen"],
    ["knowledge", "knowledge.title", "knowledge.description", "knowledge", "bilgi", "wissen"],
    ["localModels", "localModels.cardTitle", "localModels.cardDescription", "ollama", "yerel", "lokal"],
    ["helpers", "helpers.title", "helpers.description", "embeddings", "yardimci", "helfer"],
    ["providers", "settings.providers.title", "settings.providers.description", "provider", "saglayici", "anbieter"],
    ["safety-levels", "settingsModal.safety.title", "settingsModal.safety.description", "safe", "guvenli", "sicherheit"],
    ["audit-log", "audit.title", "audit.description", "audit", "gunluk", "protokoll"],
    ["target", "settingsModal.computer.target.title", "settingsModal.computer.target.description", "computer", "bilgisayar", "rechner"],
    ["my-computer", "computerAccess.title", "computerAccess.description", "screen", "ekran", "bildschirm"],
    ["trusted-folders", "folderTrust.title", "folderTrust.description", "folder", "klasor", "ordner"],
    ["allowed-grants", "settingsModal.safety.allowedGrants.title", "settingsModal.safety.allowedGrants.description", "revoke", "izin", "widerrufen"],
    ["site-rules", "settingsModal.siteRules.title", "settingsModal.siteRules.lead", "domain", "kural", "regel"],
    ["permissions", "settings.permissions.title", "settings.permissions.description", "permission", "izin", "erlaubnis"],
    ["web-search", "settingsModal.webSearch.title", "settingsModal.webSearch.description", "web search", "arama", "suche"],
    ["email", "settingsModal.email.title", "settingsModal.email.description", "mail", "e-posta", "postfach"],
    ["logs", "logs.title", "logs.privacy", "logs", "destek", "hilfe"],
    ["usage", "usage.title", "usage.subtitle", "cost", "maliyet", "verbrauch"],
    ["diagnostics", "settingsModal.diagnostics.title", "settingsModal.diagnostics.description", "repair", "onar", "reparatur"],
    ["data", "settingsModal.data.title", "settingsModal.data.description", "backup", "yedek", "sichern"],
    ["safe-mode", "safeMode.settings.title", "safeMode.settings.description", "recovery", "kurtarma", "wiederherstellung"],
    ["version", "settingsModal.updates.title", "settingsModal.updates.description", "update", "guncelleme", "aktualisierung"]
  ];

  assert.equal(cases.length, FROZEN_CARD_IDS.length);
  for (const [id, titleKey, descKey, enSyn, trSyn, deSyn] of cases) {
    const title = getKey(titleKey);
    const desc = getKey(descKey);
    const keywords = extractKeywords(id);
    assert.ok(keywords.length > 0, `expected keywords on ${id}`);
    assert.ok(matchesQuery(title, title, desc, keywords), `${id} matched by title "${title}"`);
    assert.ok(matchesQuery(enSyn, title, desc, keywords), `${id} matched by en "${enSyn}"`);
    assert.ok(matchesQuery(trSyn, title, desc, keywords), `${id} matched by tr "${trSyn}"`);
    assert.ok(matchesQuery(deSyn, title, desc, keywords), `${id} matched by de "${deSyn}"`);
  }
});
