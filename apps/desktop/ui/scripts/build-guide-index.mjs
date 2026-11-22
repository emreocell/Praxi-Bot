/* global console, process, URL */
// Builds src/help/guideIndex.json from docs/guide/*.md (the Help window searches it).
// Usage: node scripts/build-guide-index.mjs [--check]
//   --check  exit 1 when the committed file is not what the guide pages produce.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { buildGuideIndex } from "../src/lib/guideIndex.ts";

const guideDir = fileURLToPath(new URL("../../../../docs/guide/", import.meta.url));
const helpDir = fileURLToPath(new URL("../src/help/", import.meta.url));

const read = (dir, prefix) =>
  readdirSync(dir)
    .filter((n) => n.endsWith(".md"))
    .map((name) => ({ name, text: readFileSync(dir + name, "utf8") }))
    .map((f) => ({ ...f, name: prefix + f.name }));

// English pages in `guideIndex.json`; translated pages (docs/guide/<lang>/) in `guideIndex.<lang>.json`.
const outputs = [{ target: helpDir + "guideIndex.json", files: read(guideDir, "") }];
for (const lang of ["tr", "de"]) {
  outputs.push({ target: `${helpDir}guideIndex.${lang}.json`, files: read(`${guideDir}${lang}/`, `${lang}/`) });
}

let stale = false;
for (const { target, files } of outputs) {
  const next = `${JSON.stringify(buildGuideIndex(files), null, 2)}
`;
  if (process.argv.includes("--check")) {
    if (readFileSync(target, "utf8") !== next) {
      console.error(`${target} is out of date: run \`pnpm --dir apps/desktop/ui build:guide-index\``);
      stale = true;
    }
  } else {
    writeFileSync(target, next);
    console.log(`wrote ${JSON.parse(next).length} pages to ${target}`);
  }
}
if (stale) {
  process.exit(1);
}
if (process.argv.includes("--check")) {
  console.log("guide index ok");
}
