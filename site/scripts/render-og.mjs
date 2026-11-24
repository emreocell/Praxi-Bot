// Renders site/og.png (1200x630) from the SVG using headless Chrome/Edge.
// Usage: CHROME="C:/Program Files/Google/Chrome/Application/chrome.exe" node site/scripts/render-og.mjs
// Maintainer-run; the PNG is committed so CI needs no browser.
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { themeTokens } from "./build-tokens.mjs";
import { ogSvg } from "./og.mjs";

const site = join(dirname(fileURLToPath(import.meta.url)), "..");
const chrome = process.env.CHROME;
if (!chrome) throw new Error("set CHROME to a Chrome/Edge executable");
const dir = mkdtempSync(join(process.env.OG_TMP || tmpdir(), "praxi-og-"));
try {
  const svg = join(dir, "og.svg");
  writeFileSync(svg, ogSvg(themeTokens()));
  execFileSync(chrome, [
    "--headless=new",
    "--disable-gpu",
    `--user-data-dir=${join(dir, "profile")}`,
    "--hide-scrollbars",
    "--window-size=1200,630",
    `--screenshot=${join(site, "og.png")}`,
    `file:///${svg.replace(/\\/g, "/")}`,
  ]);
  console.log("wrote site/og.png");
} finally {
  rmSync(dir, { recursive: true, force: true });
}
