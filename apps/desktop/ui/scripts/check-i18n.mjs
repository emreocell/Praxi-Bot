/* global console, process, URL */
// Verifies that EN and TR have the same keys and that every static t("...") key exists.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../src/", import.meta.url));
const flat = (o, p = "") =>
  Object.entries(o).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v, `${p}${k}.`) : [`${p}${k}`]
  );
const en = new Set(flat(JSON.parse(readFileSync(join(root, "i18n/locales/en.json"), "utf8"))));
const tr = new Set(flat(JSON.parse(readFileSync(join(root, "i18n/locales/tr.json"), "utf8"))));
let bad = 0;
for (const k of en) if (!tr.has(k)) { console.error(`missing in tr: ${k}`); bad++; }
for (const k of tr) if (!en.has(k)) { console.error(`missing in en: ${k}`); bad++; }

const walk = (d) =>
  readdirSync(d).flatMap((f) => {
    const p = join(d, f);
    return statSync(p).isDirectory() ? walk(p) : /\.(tsx?|ts)$/.test(f) ? [p] : [];
  });
for (const file of walk(root)) {
  const src = readFileSync(file, "utf8");
  for (const m of src.matchAll(/\bt\(\s*"([^"$]+)"/g)) {
    if (!en.has(m[1])) { console.error(`${file}: unknown key ${m[1]}`); bad++; }
  }
}
if (bad) process.exit(1);
console.log(`i18n ok (${en.size} keys)`);
