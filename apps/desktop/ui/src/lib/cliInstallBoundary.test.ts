import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const component = readFileSync(new URL("../views/settings/CodingClisBlock.tsx", import.meta.url), "utf8");

test("CLI install UI requires executable descriptor and a single-use expiring approval", () => {
  assert.match(component, /install\?\.executable\s*\?/);
  // The install button only previews: the command is approved on the card, never by the click itself.
  assert.match(component, /onInstallLocal=\{\(id\) => void previewInstall\(id\)\}/);
  assert.match(component, /setApproval\(await localClient\.previewCliInstall\(id\)\)/);
  assert.equal(component.match(/decideCliInstall\(/g)?.length, 1);
  assert.match(component, /onDecide=\{decideInstall\}/);
  assert.match(component, /expiresAtMs=\{approval\.expires_at_ms\}/);
  assert.match(component, /approvalRemaining === 0 && !installing/);
  assert.match(component, /onClick=\{\(\) => setApproval\(null\)\}/);
  // Details suppress the shared card's standing-grant controls.
  assert.match(component, /details: \{ command: approval\.command \}/);
  // Installer output (untrusted, may carry raw node traces) is never rendered; only copied.
  assert.doesNotMatch(component, /<pre[^>]*role="log"/);
  assert.match(component, /copyDetails/);
  assert.doesNotMatch(component, /dangerouslySetInnerHTML|\binvoke\(/);
});

test("both locales carry the status strings, one status per engine", () => {
  assert.match(readFileSync(new URL("./unifiedCli.ts", import.meta.url), "utf8"), /status\.\$\{STATUS_I18N\[row\.localDetail\]\}/);
  assert.match(component, /aria-expanded=\{open\}/);
  for (const locale of ["en", "tr"]) {
    const messages = JSON.parse(readFileSync(new URL(`../i18n/locales/${locale}.json`, import.meta.url), "utf8")).settingsModal.ai.detected;
    for (const key of ["ready", "signIn", "outdated", "notInstalled", "unsupported"]) {
      assert.ok(messages.status[key], key);
    }
    assert.ok(messages.copyDetails && messages.installApproval && messages.installRunning);
  }
});
