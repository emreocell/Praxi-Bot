/* global console, process, URL */
// Builds src/help/guideIndex.json from docs/guide/*.md (the Help window searches it).
// Usage: node scripts/build-guide-index.mjs [--check]
//   --check  exit 1 when the committed file is not what the guide pages produce.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { buildGuideIndex } from "../src/lib/guideIndex.ts";

const guideDir = fileURLToPath(new URL("../../../../docs/guide/", import.meta.url));
const target = fileURLToPath(new URL("../src/help/guideIndex.json", import.meta.url));

const files = readdirSync(guideDir)
  .filter((n) => n.endsWith(".md"))
  .map((name) => ({ name, text: readFileSync(guideDir + name, "utf8") }));
const next = `${JSON.stringify(buildGuideIndex(files), null, 2)}\n`;

if (process.argv.includes("--check")) {
  const current = readFileSync(target, "utf8");
  if (current !== next) {
    console.error("src/help/guideIndex.json is out of date: run `pnpm --dir apps/desktop/ui build:guide-index`");
    process.exit(1);
  }
  console.log("guide index ok");
} else {
  writeFileSync(target, next);
  console.log(`wrote ${JSON.parse(next).length} pages`);
}
