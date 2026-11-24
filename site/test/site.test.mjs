import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { locales, shots } from "../src/content.mjs";
import { renderPage } from "../src/render.mjs";
import { summarizeRelease, isTrustedDownloadUrl } from "../src/release.mjs";
import { themeTokens } from "../scripts/build-tokens.mjs";
import { ogSvg, token } from "../scripts/og.mjs";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SHA = "a".repeat(64);
const DL = "https://github.com/emreocell/Praxi-Bot/releases/download/v1/Praxi-1-setup.exe";

const release = (over = {}) => ({
  tag_name: "v1",
  prerelease: true,
  html_url: "https://github.com/emreocell/Praxi-Bot/releases/tag/v1",
  body: "",
  assets: [{ name: "Praxi-1-setup.exe", size: 3145728, browser_download_url: DL, digest: `sha256:${"b".repeat(64)}` }],
  ...over,
});

test("release: picks the installer and the SHA-256 from the body line naming it", () => {
  const s = summarizeRelease([release({ body: `${SHA}  Praxi-1-setup.exe` })]);
  assert.equal(s.installerUrl, DL);
  assert.equal(s.sha256, SHA);
  assert.equal(s.sizeMb, 3);
});

test("release: falls back to the asset digest, never invents a hash", () => {
  assert.equal(summarizeRelease([release()]).sha256, "b".repeat(64));
  const none = release({ assets: [{ ...release().assets[0], digest: null }] });
  assert.equal(summarizeRelease([none]).sha256, null);
});

test("release: ignores drafts, foreign download hosts and garbage", () => {
  assert.equal(summarizeRelease([{ draft: true, tag_name: "v2" }]), null);
  assert.equal(summarizeRelease({ message: "rate limited" }), null);
  const evil = release({ assets: [{ name: "x-setup.exe", browser_download_url: "https://evil.example/x-setup.exe" }] });
  assert.equal(summarizeRelease([evil]).installerUrl, null);
  assert.equal(isTrustedDownloadUrl("http://github.com/emreocell/Praxi-Bot/releases/download/a/b"), false);
});

test("locales have identical keys and non-English text is really translated", () => {
  const keys = (o) => Object.keys(o).sort().join();
  assert.equal(keys(locales.en), keys(locales.tr));
  assert.equal(keys(locales.en.nav), keys(locales.tr.nav));
  assert.equal(locales.en.features.length, shots.length);
  assert.equal(locales.tr.features.length, shots.length);
  assert.notEqual(locales.tr.heroTitle, locales.en.heroTitle);
  assert.notEqual(locales.tr.smartscreen, locales.en.smartscreen);
});

test("pages: alpha badge, security link, static download fallback, no third-party loads", () => {
  for (const code of ["en", "tr"]) {
    const html = renderPage(code);
    assert.match(html, /class="badge"/);
    assert.match(html, /docs\/guide\/security\.md/);
    assert.match(html, /id="dl-button" class="btn primary" href="https:\/\/github\.com\/emreocell\/Praxi-Bot\/releases"/);
    assert.match(html, /good%20first%20issue/);
    assert.match(html, /CONTRIBUTING\.md/);
    // Only same-site assets are loaded (src/stylesheet/script); links may point to GitHub.
    const loaded = [...html.matchAll(/\b(?:src|<link rel="stylesheet" href)="([^"]+)"/g)].map((m) => m[1]);
    for (const u of loaded) assert.ok(!/^(https?:)?\/\//.test(u), `third-party load: ${u}`);
    assert.doesNotMatch(html, /google-analytics|gtag|fonts\.googleapis|cdn\./i);
  }
});

test("every screenshot and mascot referenced by the page exists in the repo", () => {
  for (const s of shots) assert.ok(existsSync(join(repo, "docs/assets/readme/screens", s.file)), s.file);
  assert.ok(existsSync(join(repo, "packages/ui/src/assets/renders/ghost.webp")));
  assert.ok(existsSync(join(repo, "site/og.png")));
});

test("social image uses the app's theme tokens", () => {
  const tokens = themeTokens();
  const svg = ogSvg(tokens);
  assert.ok(svg.includes(token(tokens, "color-background")));
  assert.ok(svg.includes(token(tokens, "color-accent")));
});
