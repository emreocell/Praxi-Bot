import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import type { i18n as I18n } from "i18next";
import { createServer, type ViteDevServer } from "vite";

// Count strings must read right for 1 and for more than 1 (#640). The real i18n setup is loaded,
// so this exercises the same `key` / `key_one` lookup the app uses.
let server: ViteDevServer;
let i18n: I18n;

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null }
  });
  ({ i18n } = await server.ssrLoadModule("/src/i18n/index.ts"));
});

after(async () => {
  await server.close();
});

type Lang = "en" | "tr" | "de";
const args = { backend: "Brave", ms: 12, used: 3, budget: 40 };
const say = (lng: Lang, key: string, count: number): string => i18n.t(key, { lng, count, ...args });

const cases: Array<{ key: string; en: [string, string]; de: [string, string] }> = [
  { key: "schedules.spec.every.days", en: ["Every day", "Every 3 days"], de: ["Jeden Tag", "Alle 3 Tage"] },
  { key: "schedules.spec.every.hours", en: ["Every hour", "Every 3 hours"], de: ["Jede Stunde", "Alle 3 Stunden"] },
  { key: "schedules.spec.every.minutes", en: ["Every minute", "Every 3 minutes"], de: ["Jede Minute", "Alle 3 Minuten"] },
  { key: "schedules.spec.every.seconds", en: ["Every second", "Every 3 seconds"], de: ["Jede Sekunde", "Alle 3 Sekunden"] },
  {
    key: "sidebar.notificationsCount",
    en: ["1 unread notification", "3 unread notifications"],
    de: ["1 ungelesene Benachrichtigung", "3 ungelesene Benachrichtigungen"]
  },
  {
    key: "bots.skills.summaryMissing",
    en: ["1 assigned skill unavailable on disk", "3 assigned skills unavailable on disk"],
    de: ["Auf dem Datenträger fehlender Skill: 1", "Auf dem Datenträger fehlende Skills: 3"]
  },
  { key: "knowledge.indexed", en: ["Processed 1 document.", "Processed 3 documents."], de: ["1 Dokument verarbeitet.", "3 Dokumente verarbeitet."] },
  { key: "knowledge.skippedFiles", en: ["1 file skipped", "3 files skipped"], de: ["1 Datei übersprungen", "3 Dateien übersprungen"] },
  { key: "knowledge.embeddingRemaining", en: ["1 chunk remaining", "3 chunks remaining"], de: ["1 Chunk übrig", "3 Chunks übrig"] },
  { key: "knowledge.embeddingSkipped", en: ["1 chunk skipped", "3 chunks skipped"], de: ["1 Chunk übersprungen", "3 Chunks übersprungen"] },
  {
    key: "usage.unknownCostNotice",
    en: [
      "1 call had unknown pricing and is excluded from the cost total.",
      "3 calls had unknown pricing and are excluded from the cost total."
    ],
    de: [
      "1 Aufruf hatte keinen bekannten Preis und ist nicht in den Gesamtkosten enthalten.",
      "3 Aufrufe hatten keinen bekannten Preis und sind nicht in den Gesamtkosten enthalten."
    ]
  },
  {
    key: "usage.unknownUsageNotice",
    en: [
      "1 call reported no token usage data and is counted separately.",
      "3 calls reported no token usage data and are counted separately."
    ],
    de: [
      "1 Aufruf hat keine Token-Daten gemeldet und wird getrennt gezählt.",
      "3 Aufrufe haben keine Token-Daten gemeldet und werden getrennt gezählt."
    ]
  },
  {
    key: "usage.limit.unpriced",
    en: ["1 call this month had no known price and is not counted.", "3 calls this month had no known price and are not counted."],
    de: [
      "1 Aufruf diesen Monat hatte keinen bekannten Preis und wurde nicht mitgezählt.",
      "3 Aufrufe diesen Monat hatten keinen bekannten Preis und wurden nicht mitgezählt."
    ]
  },
  {
    key: "botLimits.unreported",
    en: [
      "1 call reported no usage and is counted by a cautious estimate.",
      "3 calls reported no usage and are counted by a cautious estimate."
    ],
    de: [
      "1 Aufruf meldete keine Nutzung und wird mit einer vorsichtigen Schätzung gezählt.",
      "3 Aufrufe meldeten keine Nutzung und werden mit einer vorsichtigen Schätzung gezählt."
    ]
  },
  {
    key: "botEdit.deleteBlockedRuns",
    en: [
      "This bot still has 1 retained run, so it cannot be deleted. Only a bot without chats or runs can be removed.",
      "This bot still has 3 retained runs, so it cannot be deleted. Only a bot without chats or runs can be removed."
    ],
    de: [
      "Dieser Bot hat noch einen gespeicherten Lauf (1) und kann daher nicht gelöscht werden. Nur ein Bot ohne Chats oder Läufe kann entfernt werden.",
      "Dieser Bot hat noch gespeicherte Läufe (3) und kann daher nicht gelöscht werden. Nur ein Bot ohne Chats oder Läufe kann entfernt werden."
    ]
  },
  {
    key: "settingsModal.diagnostics.problems",
    en: ["1 item needs attention", "3 items need attention"],
    de: ["1 Punkt braucht Aufmerksamkeit", "3 Punkte brauchen Aufmerksamkeit"]
  },
  { key: "localModels.cores", en: ["1 processor thread", "3 processor threads"], de: ["1 Prozessor-Thread", "3 Prozessor-Threads"] },
  { key: "widget.refusedBadge", en: ["1 bot action refused", "3 bot actions refused"], de: ["1 Bot-Aktion abgelehnt", "3 Bot-Aktionen abgelehnt"] },
  {
    key: "settingsModal.webSearch.testOk",
    en: ["Brave answered (1 result in 12 ms)", "Brave answered (3 results in 12 ms)"],
    de: ["Brave hat geantwortet (1 Ergebnis in 12 ms)", "Brave hat geantwortet (3 Ergebnisse in 12 ms)"]
  },
  {
    key: "memory.previewTokens",
    en: ["1 memory recalled · ~3 / 40 tokens", "3 memories recalled · ~3 / 40 tokens"],
    de: ["1 Erinnerung abgerufen · ~3 / 40 Tokens", "3 Erinnerungen abgerufen · ~3 / 40 Tokens"]
  }
];

test("English and German count strings read right for 1 and for 3", () => {
  for (const c of cases) {
    assert.equal(say("en", c.key, 1), c.en[0], `en ${c.key} (1)`);
    assert.equal(say("en", c.key, 3), c.en[1], `en ${c.key} (3)`);
    assert.equal(say("de", c.key, 1), c.de[0], `de ${c.key} (1)`);
    assert.equal(say("de", c.key, 3), c.de[1], `de ${c.key} (3)`);
  }
});

test("every key has a singular variant in all three languages", () => {
  for (const lng of ["en", "tr", "de"] as const) {
    for (const c of cases) {
      assert.ok(i18n.exists(`${c.key}_one`, { lng }), `${lng} is missing ${c.key}_one`);
    }
  }
});

test("Turkish keeps its natural cadence wording for one unit", () => {
  assert.equal(say("tr", "schedules.spec.every.days", 1), "Her gün");
  assert.equal(say("tr", "schedules.spec.every.hours", 1), "Her saat");
  assert.equal(say("tr", "schedules.spec.every.minutes", 1), "Her dakika");
  assert.equal(say("tr", "schedules.spec.every.seconds", 1), "Her saniye");
  assert.equal(say("tr", "schedules.spec.every.hours", 3), "Her 3 saatte bir");
});

test("no count string is left with a '(s)' placeholder or a bare 1-plural", () => {
  for (const lng of ["en", "de"] as const) {
    for (const c of cases) {
      for (const n of [1, 3]) {
        const text = say(lng, c.key, n);
        assert.doesNotMatch(text, /\(s\)|\(e\)/, `${lng} ${c.key} (${n})`);
      }
    }
  }
});
