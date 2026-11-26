import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execPath } from "node:process";
import { test } from "node:test";

const checker = new URL("../../scripts/check-i18n.mjs", import.meta.url);
const englishFallback = new URL("../i18n/englishFallback.ts", import.meta.url);

const translation = (key: string) => ["t", "(", JSON.stringify(key), ")"].join("");

function check(
  locales: Record<string, Record<string, unknown>>,
  source: Record<string, string> = {},
  scaffold: string[] = [],
) {
  const root = mkdtempSync(join(tmpdir(), "praxi-i18n-"));
  try {
    const scriptsDir = join(root, "scripts");
    const srcDir = join(root, "src");
    const localeDir = join(srcDir, "i18n", "locales");
    mkdirSync(scriptsDir);
    mkdirSync(localeDir, { recursive: true });
    const script = join(scriptsDir, "check-i18n.mjs");
    copyFileSync(checker, script);
    copyFileSync(englishFallback, join(srcDir, "i18n", "englishFallback.ts"));
    for (const [name, content] of Object.entries(locales)) {
      writeFileSync(join(localeDir, name + ".json"), JSON.stringify(content));
    }
    if (scaffold.length) {
      writeFileSync(join(srcDir, "i18n", "scaffold-locales.json"), JSON.stringify({ scaffold }));
    }
    for (const [file, content] of Object.entries(source)) {
      writeFileSync(join(srcDir, file), content);
    }
    return spawnSync(execPath, [script], { encoding: "utf8" });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("checks newly added locales against English and names missing and extra keys", () => {
  const en = { chat: { send: "Send" } };
  const tr = { chat: { send: "Gönder" } };
  const result = check({ en, tr, de: { chat: { cancel: "Abbrechen" } } });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /missing in de: chat\.send/);
  assert.match(result.stderr, /extra in de: chat\.cancel/);
});

test("a scaffold locale only warns about missing keys; a strict one fails", () => {
  const en = { chat: { send: "Send", stop: "Stop" } };
  const lagging = { chat: { send: "Kirim" } };
  const scaffold = check({ en, id: lagging }, {}, ["id"]);
  assert.equal(scaffold.status, 0);
  assert.equal(scaffold.stdout.trim(), "i18n ok (2 keys)");
  assert.match(scaffold.stderr, /warn missing in id \(scaffold\): chat\.stop/);
  const strict = check({ en, id: lagging });
  assert.equal(strict.status, 1);
  assert.match(strict.stderr, /missing in id: chat\.stop/);
});

test("the hub uses English fallback in German and Spanish while Turkish stays required", () => {
  const en = { apps: { composioHub: { linkAccount: "Connect Composio" } } };
  const tr = { apps: { composioHub: { linkAccount: "Composio hesabını bağla" } } };
  assert.equal(check({ en, tr, de: {}, es: {} }).status, 0);
  const missingTurkish = check({ en, tr: {}, de: {}, es: {} });
  assert.equal(missingTurkish.status, 1);
  assert.match(missingTurkish.stderr, /missing in tr: apps\.composioHub\.linkAccount/);
});

test("reports a static unknown key once, independent of the number of locales", () => {
  const result = check(
    { en: { send: "Send" }, tr: { send: "Gönder" }, de: { send: "Senden" } },
    { "a.ts": translation("unknown"), "b.tsx": translation("unknown") },
  );
  assert.equal(result.status, 1);
  assert.equal(result.stderr.match(/unknown key unknown/g)?.length, 1);
});

test("keeps the success output when all locale and source keys match", () => {
  const result = check(
    { en: { send: "Send" }, tr: { send: "Gönder" }, de: { send: "Senden" } },
    { "a.ts": translation("send") },
  );
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), "i18n ok (1 keys)");
});

test("warns about values identical to English without changing the output or exit code", () => {
  const result = check({
    en: { send: "Send", port: "Settings" },
    tr: { send: "Gönder", port: "Settings" },
  });
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), "i18n ok (2 keys)");
  assert.match(result.stderr, /^tr: 1 values identical to English$/m);
  assert.match(result.stderr, /^warn tr: port = "Settings"$/m);
  assert.doesNotMatch(result.stderr, /warn tr: send/);
});

test("skips keyboard shortcut combinations but still warns about a bare key name", () => {
  const keys = { combo: "Ctrl+Enter", tab: "Tab" };
  const result = check({ en: { keys }, tr: { keys } });
  assert.equal(result.status, 0);
  assert.match(result.stderr, /^warn tr: keys\.tab = "Tab"$/m);
  assert.doesNotMatch(result.stderr, /keys\.combo/);
});

test("a word that is a cognate in German is still flagged in Turkish", () => {
  const result = check({
    en: { where: "Details" },
    tr: { where: "Details" },
    de: { where: "Details" },
  });
  assert.equal(result.status, 0);
  assert.match(result.stderr, /^warn tr: where = "Details"$/m);
  assert.doesNotMatch(result.stderr, /warn de:/);
});
