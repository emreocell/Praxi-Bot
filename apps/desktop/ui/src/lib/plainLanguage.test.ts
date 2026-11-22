import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Plain-language guard for people who are not programmers. Words that only make sense to
 * developers must not appear in user-facing strings. Advanced and diagnostic screens may name
 * the technical thing (their key prefixes are in ADVANCED_ALLOWED, each with its reason).
 */

const BANNED: Array<[string, RegExp]> = [
  ["daemon", /\bdaemons?\b/i],
  ["peer", /\bpeers?\b/i],
  ["egress", /\begress\b/i],
  ["taint", /\btaint(?:ed|s)?\b/i],
  ["payload", /\bpayloads?\b/i],
  ["endpoint", /\b(?:endpoints?|endpunkte?n?)\b|\buç nokta/i],
  ["sandbox", /\bsandbox(?:ed)?\b/i],
  ["latency", /\b(?:latency|latenz)\b/i],
  ["handshake", /\bhandshake\b/i],
  ["credentials", /\bcredentials?\b/i],
  ["stdout/stderr", /\bstd(?:out|err)\b/i],
  ["loopback", /\b(?:loopback|localhost)\b/i],
  ["token", /\btokens?\b|\bjeton/i],
  ["http status", /\b(?:HTTP\s?\d{3}|40[0-9]|41[0-9]|42[0-9]|50[0-3])\b(?!\s?(?:KB|MB|ms|px))/]
];

/** Advanced / diagnostic screens, and product names, where the technical word is the point. */
const ADVANCED_ALLOWED = [
  "settingsModal.diagnostics.", // support report and health checks
  "settingsModal.email.", // mail server settings (TLS)
  "settingsModal.updates.", // version rows
  "knowledge.", // advanced knowledge-base settings
  "usage.", // the usage screen reports tokens by design
  "messageInfo.rows.", // message details panel
  "memory.previewTokens",
  "pastedSecret.kind.", // names of the products a pasted key belongs to
  "apps.advanced.", // "Advanced" apps section
  "chat.route.hover", // routing details shown on hover
  "chatHistory.exportJson",
  "health.details"
];

const dir = fileURLToPath(new URL("../i18n/locales/", import.meta.url));
const flat = (o: Record<string, unknown>, p = ""): Array<[string, string]> =>
  Object.entries(o).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v as Record<string, unknown>, `${p}${k}.`) : typeof v === "string" ? [[`${p}${k}`, v] as [string, string]] : []
  );

for (const lng of ["en", "tr", "de"]) {
  test(`${lng}: user-facing strings avoid developer jargon`, () => {
    const entries = flat(JSON.parse(readFileSync(`${dir}${lng}.json`, "utf8")));
    const hits: string[] = [];
    for (const [key, value] of entries) {
      if (ADVANCED_ALLOWED.some((prefix) => key.startsWith(prefix))) {
        continue;
      }
      for (const [word, re] of BANNED) {
        if (re.test(value)) {
          hits.push(`${key}: "${word}" in "${value.slice(0, 80)}"`);
        }
      }
    }
    assert.deepEqual(hits, [], `jargon in ${lng}:\n${hits.join("\n")}`);
  });
}

test("the jargon list really catches jargon", () => {
  assert.ok(BANNED.some(([, re]) => re.test("The daemon returned 409")));
  assert.ok(BANNED.some(([, re]) => re.test("Der Daemon antwortet nicht")));
  assert.ok(!BANNED.some(([, re]) => re.test("The background service is starting. GIFs up to 400 KB.")));
});
