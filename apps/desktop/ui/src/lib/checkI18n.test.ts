import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execPath } from "node:process";
import { test } from "node:test";

const checker = new URL("../../scripts/check-i18n.mjs", import.meta.url);

const translation = (key: string) => ["t", "(", JSON.stringify(key), ")"].join("");

function check(
  locales: Record<string, Record<string, unknown>>,
  source: Record<string, string> = {},
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
    for (const [name, content] of Object.entries(locales)) {
      writeFileSync(join(localeDir, name + ".json"), JSON.stringify(content));
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
