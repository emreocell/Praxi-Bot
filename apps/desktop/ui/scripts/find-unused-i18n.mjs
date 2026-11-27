/* global console, process */
// Lists locale keys that have no static or modeled dynamic use in the UI sources.
// Dynamic patterns: template-literal prefixes, humanError codes, live-view families, etc.
// Keys under englishFallbackPrefixes are never reported (staged English-first copy).
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const UI_ROOT = fileURLToPath(new URL("../", import.meta.url));
const REPO_ROOT = fileURLToPath(new URL("../../../../", import.meta.url));
const LOCALE_DIR = join(UI_ROOT, "src/i18n/locales");
const SCAFFOLD_FILE = join(UI_ROOT, "src/i18n/scaffold-locales.json");
const PREREQ_RS = join(REPO_ROOT, "apps/desktop/src/prereq.rs");

const KEY_LIKE = /^[a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+$/;

const flat = (o, p = "") =>
  Object.entries(o).flatMap(([k, v]) =>
    v && typeof v === "object" ? flat(v, `${p}${k}.`) : [[`${p}${k}`, v]]
  );

const walkFiles = (root, pred) => {
  const out = [];
  const walk = (d) => {
    for (const f of readdirSync(d)) {
      const p = join(d, f);
      if (statSync(p).isDirectory()) {
        if (!f.includes("i18n") || !p.includes(`${join("src", "i18n", "locales")}`)) walk(p);
      } else if (pred(p)) out.push(p);
    }
  };
  walk(root);
  return out;
};

/** @typedef {{ prefix: string, suffix?: string, infix?: boolean }} PrefixRule */

/** i18n key string literals in maps and return paths (not every dotted string in the repo). */
function extractStringLiteralKeys(src) {
  const keys = new Set();
  const lineHints =
    /\b(?:i18nKey|detailKey|fixKey|extraKey|labelKey|chip\.label|key:|return)\b/;
  for (const line of src.split("\n")) {
    if (!lineHints.test(line)) continue;
    for (const m of line.matchAll(/["']([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+)["']/g)) {
      if (KEY_LIKE.test(m[1])) keys.add(m[1]);
    }
  }
  for (const m of src.matchAll(/^\s+[a-zA-Z]+:\s*"([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+)",?\s*$/gm)) {
    keys.add(m[1]);
  }
  return keys;
}

/** @param {string} src @param {Set<string>} allKeys */
function extractStaticTKeys(src, allKeys) {
  const keys = new Set();
  const re = /\b(?:t|i18n\.t|tr)\(\s*["']([^"'$]+)["']/g;
  for (const m of src.matchAll(re)) keys.add(m[1]);
  // t(cond ? "key.a" : "key.b") and t(dynamicVar) tables resolved elsewhere.
  const callRe = /\b(?:t|i18n\.t|tr)\(\s*([^;]+?)\s*(?:,|\))/g;
  for (const m of src.matchAll(callRe)) {
    for (const s of m[1].matchAll(/["']([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+)["']/g)) {
      if (allKeys.has(s[1])) keys.add(s[1]);
    }
  }
  for (const m of src.matchAll(/\b(?:key|kbd):\s*["']([^"']+)["']/g)) keys.add(m[1]);
  for (const m of src.matchAll(/^\s+["']([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+)["']\s*,?\s*$/gm)) {
    if (allKeys.has(m[1])) keys.add(m[1]);
  }
  return keys;
}

/**
 * From `t(\`foo.bar.${x}.title\`)` → { prefix: "foo.bar.", suffix: ".title", infix: true }.
 * @param {string} src
 * @returns {PrefixRule[]}
 */
function extractTemplateRules(src) {
  const rules = [];
  const re = /\b(?:t|i18n\.t|tr)\(\s*`([^`]+)`/g;
  const returnRe = /return\s*`([^`]+)`/g;
  for (const m of src.matchAll(re)) {
    const body = m[1];
  const i = body.indexOf("${");
    if (i === -1) {
      if (KEY_LIKE.test(body)) rules.push({ prefix: body, suffix: undefined, infix: false });
      continue;
    }
    const prefix = body.slice(0, i);
    const after = body.slice(body.lastIndexOf("}") + 1);
    const infix = (body.match(/\$\{/g) ?? []).length > 0;
    rules.push({
      prefix,
      suffix: after || undefined,
      infix: infix || prefix.includes(".")
    });
  }
  for (const m of src.matchAll(returnRe)) {
    const body = m[1];
    const i = body.indexOf("${");
    if (i === -1) {
      if (KEY_LIKE.test(body)) rules.push({ prefix: body, suffix: undefined, infix: false });
      continue;
    }
    const prefix = body.slice(0, i);
    const after = body.slice(body.lastIndexOf("}") + 1);
    rules.push({ prefix, suffix: after || undefined, infix: true });
  }
  for (const m of src.matchAll(/`([^`]*\$\{[^`]*)`/g)) {
    const body = m[1];
    const i = body.indexOf("${");
    if (i < 1) continue;
    const prefix = body.slice(0, i);
    if (!/^[a-z][a-zA-Z0-9]*(\.[a-zA-Z0-9_]+)+/.test(prefix)) continue;
    const after = body.slice(body.lastIndexOf("}") + 1);
    rules.push({ prefix, suffix: after || undefined, infix: !after });
  }
  return rules;
}

/** @param {string} file */
function readModule(file) {
  return readFileSync(join(UI_ROOT, file), "utf8");
}

/** @param {Set<string>} used */
function addHumanErrorKeys(used) {
  const src = readModule("src/lib/humanError.ts");
  const mark = (key) => {
    used.add(`${key}.title`);
    used.add(`${key}.hint`);
  };
  const arrayStart = src.indexOf("export const DAEMON_ERROR_CODES");
  const arrayEnd = src.indexOf("] as const", arrayStart);
  const daemonBlock = src.slice(arrayStart, arrayEnd);
  for (const m of daemonBlock.matchAll(/"([a-z0-9_]+)"/g)) {
    mark(`errors.codes.${m[1]}`);
  }
  const patternBlock = src.match(/const PATTERNS[\s\S]*?^\];/m)?.[0] ?? "";
  for (const m of patternBlock.matchAll(/,\s*"([^"]+)"/g)) {
    const key = m[1];
    if (key.startsWith("codes.")) mark(`errors.${key}`);
    else mark(`errors.${key}`);
  }
  const reasonBlock = src.match(/const REASONS[\s\S]*?^\];/m)?.[0] ?? "";
  for (const m of reasonBlock.matchAll(/,\s*"([^"]+)"\s*\]/g)) mark(`errors.${m[1]}`);
  const engineBlock = src.match(/const ENGINE_REASONS[\s\S]*?^\];/m)?.[0] ?? "";
  for (const m of engineBlock.matchAll(/,\s*"([^"]+)"\s*\]/g)) mark(`errors.${m[1]}`);

  mark("errors.generic");
  mark("errors.network");
  mark("errors.http_error");
  mark("errors.toolFailed");
  mark("errors.serviceConnection");
  mark("errors.hostSilent");
  mark("errors.desktop_only");
  for (const k of ["serviceStarting", "serviceDown", "serviceWrongTarget"]) mark(`errors.${k}`);
  for (const k of ["provider_endpoint", "provider_model"]) mark(`errors.codes.${k}`);

  const startupCases = [...src.matchAll(/case "[^"]+": return "([^"]+)";/g)];
  const inStartup = (idx) => src.slice(idx - 80, idx).includes("startupKey");
  for (const m of startupCases) {
    if (inStartup(m.index)) {
      mark(`errors.startup.${m[1]}`);
    }
  }
  const exitKinds = [...src.matchAll(/:\s*"(exited[^"]+)"/g)];
  for (const m of exitKinds) mark(`errors.startup.${m[1]}`);

  for (const kind of ["auth", "forbidden", "quota", "rateLimited", "transient", "taken", "rejected", "other"]) {
    mark(`errors.emailProvider.${kind}`);
  }
}

/** @param {Set<string>} used */
function addHostModeKeys(used) {
  const src = readModule("src/lib/hostMode.ts");
  for (const m of src.matchAll(/\[\s*"[^"]+",\s*"([^"]+)"\s*\]/g)) {
    used.add(`hostMode.refusal.${m[1]}`);
  }
  used.add("hostMode.refusal.needsAdmin");
  used.add("hostMode.refusal.adminRequired");
  used.add("hostMode.refusal.generic");
}

/** @param {Set<string>} used */
function addScreenKeys(used) {
  const label = readModule("src/lib/computerLabel.ts");
  for (const m of label.matchAll(/:\s*"([^"]+)"/g)) {
    const v = m[1];
    if (KEY_LIKE.test(v)) used.add(v);
  }
  const noScreenBlock = label.match(/NO_SCREEN_CODES[\s\S]*?^\] as const/m)?.[0] ?? "";
  for (const m of noScreenBlock.matchAll(/"([a-z_]+)"/g)) {
    used.add(`liveView.noScreen.${m[1]}`);
  }
  used.add("liveView.noScreen.unknown");

  const screen = readModule("src/lib/screenReason.ts");
  for (const m of screen.matchAll(/:\s*"([^"]+)"/g)) {
    if (KEY_LIKE.test(m[1])) used.add(m[1]);
  }
  for (const m of screen.matchAll(/`liveView\.noScreenFix\.([^`]+)`/g)) {
    used.add(`liveView.noScreenFix.${m[1]}`);
  }
}

/** @param {Set<string>} used */
function addAccessLevelKeys(used, allKeys) {
  const src = readModule("src/lib/accessLevels.ts");
  const block = src.match(/export const ACCESS_LEVELS[\s\S]*?^\] as const/m)?.[0] ?? "";
  for (const m of block.matchAll(/"([a-z][a-z0-9-]*)"/g)) {
    const prefix = `accessLevel.${m[1]}.`;
    for (const k of allKeys) {
      if (k.startsWith(prefix)) used.add(k);
    }
  }
  for (const preset of ["custom"]) {
    const prefix = `accessLevel.${preset}.`;
    for (const k of allKeys) {
      if (k.startsWith(prefix)) used.add(k);
    }
  }
}

/** @param {Set<string>} used */
function addPrereqKeys(used) {
  const rust = readFileSync(PREREQ_RS, "utf8");
  for (const m of rust.matchAll(/id:\s*"([a-z0-9_]+)"/g)) {
    used.add(`errors.prereq.${m[1]}.title`);
    used.add(`errors.prereq.${m[1]}.hint`);
  }
  used.add("errors.prereq.download");
  used.add("errors.prereq.openDataFolder");
  used.add("errors.prereq.advanced");
  used.add("errors.prereq.detailsTitle");
}

/** @param {string} key @param {PrefixRule[]} rules */
function matchesPrefixRule(key, rules) {
  for (const r of rules) {
    // `t(\`${K}.card.title\`)` — suffix-only rules are not applied globally.
    if (!r.prefix) continue;
    if (!key.startsWith(r.prefix)) continue;
    if (r.suffix) {
      const ok =
        key.endsWith(r.suffix) || new RegExp(`${r.suffix.replaceAll(".", "\\.")}\\.\\d+$`).test(key);
      if (!ok) continue;
    }
    return true;
  }
  return false;
}

/** `const K = "settingsModal.connectMail"` + `` t(`${K}.card.title`) `` */
function expandConstKeyPrefixes(src, used, allKeys) {
  const consts = new Map();
  for (const m of src.matchAll(/const\s+([A-Z_][A-Z0-9_]*)\s*=\s*"([^"]+)"\s*;/g)) {
    consts.set(m[1], m[2]);
  }
  for (const m of src.matchAll(/`[^`]*\$\{([A-Z_][A-Z0-9_]*)\}([^`]*)`/g)) {
    const base = consts.get(m[1]);
    if (!base || !KEY_LIKE.test(`${base}${m[2]}`) && !m[2].startsWith(".")) continue;
    const literal = `${base}${m[2]}`;
    if (allKeys.has(literal)) used.add(literal);
    const prefix = literal.endsWith(".") ? literal : `${literal}.`;
    if (m[2] === "" || m[2].startsWith(".")) {
      for (const k of allKeys) {
        if (k.startsWith(base + (m[2] || "."))) used.add(k);
      }
    }
  }
}

/**
 * @param {object} opts
 * @param {string} [opts.localeDir]
 * @param {string} [opts.scanRoot]
 */
export function findUnusedKeys({
  localeDir = LOCALE_DIR,
  scanRoot = UI_ROOT,
} = {}) {
  const en = JSON.parse(readFileSync(join(localeDir, "en.json"), "utf8"));
  const allKeys = new Set(flat(en).map(([k]) => k));

  let protectedPrefixes = [];
  try {
    const policy = JSON.parse(readFileSync(SCAFFOLD_FILE, "utf8"));
    protectedPrefixes = policy.englishFallbackPrefixes ?? [];
  } catch {
    protectedPrefixes = [];
  }
  // Daemon / live-view families: do not delete without a proven static miss (PR #1000).
  protectedPrefixes = [
    ...protectedPrefixes,
    "liveView.errors.",
    "liveView.reasons.",
    "liveView.noScreen.",
    "liveView.noScreenFix.",
    "errors.codes.",
    "errors.startup.",
    "errors.prereq.",
    "hostMode.refusal.",
  ];

  const used = new Set();
  const prefixRules = [];

  const roots = [
    join(scanRoot, "src"),
    join(scanRoot, "tests"),
    join(scanRoot, "scripts"),
  ].filter((d) => {
    try {
      return statSync(d).isDirectory();
    } catch {
      return false;
    }
  });

  const files = roots.flatMap((r) =>
    walkFiles(r, (p) => /\.(tsx?|mjs)$/.test(p) && !p.includes(`${join("i18n", "locales")}`))
  );

  for (const file of files) {
    if (file.endsWith("find-unused-i18n.mjs")) continue;
    const src = readFileSync(file, "utf8");
    for (const k of extractStaticTKeys(src, allKeys)) used.add(k);
    for (const k of extractStringLiteralKeys(src)) {
      if (allKeys.has(k)) used.add(k);
    }
    for (const m of src.matchAll(/["']([a-z][a-zA-Z0-9]*(?:\.[a-zA-Z0-9_]+)+)["']/g)) {
      if (allKeys.has(m[1])) used.add(m[1]);
    }
    prefixRules.push(...extractTemplateRules(src));
    for (const m of src.matchAll(/\bi18nKey:\s*["']([^"']+)["']/g)) used.add(m[1]);
    for (const m of src.matchAll(/accessLevelKey\([^)]+\)\}\.([a-z]+)`/g)) {
      for (const preset of ["full", "approve-destructive", "read-and-draft", "read-only", "custom"]) {
        used.add(`accessLevel.${preset}.${m[1]}`);
      }
    }
  }

  addHumanErrorKeys(used);
  addHostModeKeys(used);
  addScreenKeys(used);
  addPrereqKeys(used);
  addAccessLevelKeys(used, allKeys);

  for (const file of files) {
    if (file.endsWith("find-unused-i18n.mjs")) continue;
    expandConstKeyPrefixes(readFileSync(file, "utf8"), used, allKeys);
  }

  // Prefix rules from templates: any locale key that fits is considered used.
  for (const key of allKeys) {
    if (matchesPrefixRule(key, prefixRules)) used.add(key);
  }

  // i18next plural suffixes: base key used via `chat.${name}` implies *_one/_other siblings.
  for (const key of [...used]) {
    if (allKeys.has(`${key}_one`)) used.add(`${key}_one`);
    if (allKeys.has(`${key}_other`)) used.add(`${key}_other`);
    if (allKeys.has(`${key}_zero`)) used.add(`${key}_zero`);
  }

  // Array values via returnObjects: parent key marks numeric children (firstRun…examples.0).
  for (const key of [...used]) {
    if (!allKeys.has(`${key}.0`)) continue;
    for (const child of allKeys) {
      if (child.startsWith(`${key}.`) && /^\d+$/.test(child.slice(key.length + 1))) used.add(child);
    }
  }

  const unused = [...allKeys].filter((key) => {
    if (used.has(key)) return false;
    if (protectedPrefixes.some((p) => key.startsWith(p))) return false;
    return true;
  });

  unused.sort();
  return { unused, usedCount: used.size, total: allKeys.size, protectedPrefixes };
}

/** @param {object} obj @param {string} dotted */
function deleteKey(obj, dotted) {
  const parts = dotted.split(".");
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    if (!cur || typeof cur !== "object") return;
    cur = cur[parts[i]];
  }
  if (cur && typeof cur === "object") delete cur[parts[parts.length - 1]];
}

/** @param {string[]} keys @param {string} [localeDir] */
export function removeKeysFromLocales(keys, localeDir = LOCALE_DIR) {
  const files = readdirSync(localeDir).filter((f) => f.endsWith(".json"));
  for (const file of files) {
    const path = join(localeDir, file);
    const json = JSON.parse(readFileSync(path, "utf8"));
    for (const key of keys) deleteKey(json, key);
    writeFileSync(path, `${JSON.stringify(json, null, 2)}\n`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const apply = process.argv.includes("--apply");
  const jsonOut = process.argv.includes("--json");
  const { unused, usedCount, total, protectedPrefixes } = findUnusedKeys();

  if (jsonOut) {
    console.log(JSON.stringify({ unused, usedCount, total, protectedPrefixes }, null, 2));
  } else {
    console.log(`Scanned ${total} English keys; ${usedCount} marked used; ${unused.length} unused candidates`);
    for (const k of unused) console.log(k);
  }

  if (apply) {
    if (unused.length === 0) {
      console.log("Nothing to remove.");
    } else {
      removeKeysFromLocales(unused);
      console.log(`Removed ${unused.length} keys from all locale files under ${relative(process.cwd(), LOCALE_DIR)}`);
    }
  }
}