import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { test } from "node:test";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
const block = read("./CodingClisBlock.tsx");
const home = read("./HomeCards.tsx");

test("server administration only ever goes through the server's own channel", () => {
  // No fall-back to this computer's daemon: it would install and sign in on the wrong machine.
  const hook = home.slice(home.indexOf("export function useHostAdmin"), home.indexOf("export function HostAdminAlert"));
  assert.doesNotMatch(hook, /return client;/);
  assert.match(hook, /active\.isHost \? client : ownsChannel \? own : null/);
  assert.match(hook, /made\.dispose\(\)/);
  const install = home.slice(home.indexOf("export function useServerInstall"), home.indexOf("export function InstallFeedback"));
  assert.doesNotMatch(install, /useDaemon\(\)/);
  assert.match(install, /client: ApiClient \| null/);
  const modal = read("../../components/ServerSignInModal.tsx");
  assert.doesNotMatch(modal, /const \{[^}]*\bclient\b[^}]*\} = useDaemon\(\)/);
  assert.match(block, /client=\{hostApi\}/);
  // This computer's list comes from the local daemon, also while a server is the home.
  assert.match(block, /useLocalDaemonClient\(\)/);
});

test("no reload loop: rows and the block depend on the stable reload callback", () => {
  assert.match(block, /\[loadLocal, showServer, reloadHost\]/);
  assert.match(home, /changed\.current\?\.\(\)/);
  assert.match(home, /\}, \[finished\]\);/);
});

test("the old per-block actions are all still reachable", () => {
  for (const needle of [
    "<InstallFeedback",
    "<RunOnServer",
    "copyInstallLocal",
    "copyLoginLocal",
    "copyInstallServer",
    "copyLoginServer",
    "engine-path-",
    "coding-cli-path-",
    "home.admin.byDesign",
    "home.engines.runsAs",
    "noteUnverified",
    "experimentalChip"
  ]) {
    assert.ok(block.includes(needle), needle);
  }
});

test("row menu: ARIA-valid children, focus returns to its button, deep link lands on the button", () => {
  assert.match(block, /<div className=\{styles\.cliMenu\} role="none">/);
  assert.doesNotMatch(block, /role="presentation"/);
  assert.match(block, /menuBtn\.current\?\.focus\(\)/);
  assert.match(block, /beforeOpen=\{onDone\}/);
  const li = block.match(/<li[^>]*>/)?.[0] ?? "";
  assert.doesNotMatch(li, /data-settings-field/);
  assert.match(block, /data-settings-field=\{field\}/);
});

test("the model picker closes itself before it opens Settings", () => {
  const picker = read("../../components/ModelPicker.tsx");
  assert.match(picker, /onSetUp\(\);\s*openSettingsTarget\(/);
});

test("quota samples only exist in mock mode", () => {
  const quota = read("../../state/useCodingCliQuota.ts");
  assert.match(quota, /!isMockMode\(\)/);
  assert.equal(quota.match(/CACHE_MS\) \{/g)?.length, 1);
});

test("the dead blocks are gone", () => {
  assert.equal(existsSync(new URL("./DetectedAccountsBlock.tsx", import.meta.url)), false);
  for (const name of ["HostEnginesBlock", "HostEngineRow", "EngineSetup"]) {
    assert.doesNotMatch(home, new RegExp(name));
  }
});

test("the u17 screenshots hold only app captures and no local paths", () => {
  const dir = new URL("../../../../../../docs/screenshots/u17/", import.meta.url);
  const files = readdirSync(dir);
  assert.ok(!files.some((f) => /^fb\d/.test(f)), "owner reference shots must not be committed");
  assert.doesNotMatch(readFileSync(new URL("README.md", dir), "utf8"), /\/srv\//);
});
