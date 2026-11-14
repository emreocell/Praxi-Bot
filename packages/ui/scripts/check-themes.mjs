import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { URL } from 'node:url';
import process from 'node:process';
import {
  PALETTES, FAVORITE_THEMES, DEFAULT_CUSTOM_THEME, UI_STYLES, buildTokens, contrastRatio, deriveTheme,
  fallbackAccent, inkOn, mix, normalizeHex, paletteById
} from '../src/themeModel.ts';

// Every shipped theme must be readable.  The derivation pulls faded text, status colours and
// accent text toward the text colour until they pass, so the hard requirement is the one
// the derivation cannot repair: the theme's own text colour on bg / surface / raised.
const root = new URL('../', import.meta.url);
const css = readFileSync(new URL('tokens.css', root), 'utf8');
assert.equal(PALETTES.length, 29, 'the prototype ships 29 themes');
assert.equal(new Set(PALETTES.map((p) => p.id)).size, PALETTES.length, 'theme ids are unique');
for (const id of FAVORITE_THEMES) assert.ok(paletteById(id), `favourite ${id} exists`);

const BOT_COLOR_SAMPLES = ['#3b82f6', '#10b981', '#8b5cf6', '#f59e0b', '#e11d48', '#0ea5e9', '#ffffff', '#000000', '#808080'];
const failures = [];
const check = (name, label, fg, bg, min) => {
  const r = contrastRatio(fg, bg);
  if (r < min) failures.push(`${name}: ${label} ${r.toFixed(2)} < ${min}`);
  return r;
};

for (const theme of [...PALETTES, { id: 'custom (default)', ...DEFAULT_CUSTOM_THEME }]) {
  const d = deriveTheme(theme);
  const name = theme.id;
  const surfaces = { background: d.bg, surface: d.surface, raised: d.raised, hover: d.hover };
  let lowest = Infinity;
  for (const [sname, s] of Object.entries(surfaces)) {
    const required = sname === 'hover' ? 0 : 4.5; // text on bg / surface / raised is the contract
    const r = check(name, `text/${sname}`, d.text, s, required);
    if (sname !== 'hover') lowest = Math.min(lowest, r);
    for (const [label, color] of [['secondary', d.secondary], ['muted', d.muted], ['success', d.success], ['warning', d.warning], ['danger', d.danger]]) {
      check(name, `${label}/${sname}`, color, s, 4.5);
    }
    check(name, `focus/${sname}`, d.focus, s, sname === 'hover' ? 0 : 3);
    check(name, `control border/${sname}`, d.lineStrong, s, sname === 'hover' ? 0 : 3);
  }
  for (const style of UI_STYLES) {
    const t = buildTokens(theme, theme.accent ?? fallbackAccent(d), style, false);
    for (const key of ['--color-surface', '--color-surface-elevated', '--color-border']) assert.ok(t[key], `${name}/${style}: ${key}`);
  }
  // Accents: the theme's own, then arbitrary bot colours.
  for (const accent of [theme.accent ?? fallbackAccent(d), ...BOT_COLOR_SAMPLES]) {
    const t = buildTokens(theme, accent, 'solid', false);
    const a = normalizeHex(accent);
    check(name, `accent button ${a}`, inkOn(a), a, 4.5);
    check(name, `accent button hover ${a}`, inkOn(a), t['--color-accent-hover'], 4.5);
    for (const s of [d.bg, d.surface, d.raised]) check(name, `accent text ${a}`, t['--color-accent-text'], s, 4.5);
    for (const token of ['text', 'keyword', 'string', 'number', 'comment']) {
      check(name, `code ${token}`, t[`--color-code-${token === 'text' ? 'text' : token}`], d.bg, 4.5);
    }
  }
  check(name, 'danger button', d.dangerInk, d.danger, 4.5);
  for (const status of ['success', 'warning', 'danger']) {
    check(name, `${status} badge`, d[status], mix(d[status], d.surface, 0.9), 4.5);
  }
  process.stdout.write(`${name}: ${d.light ? 'light' : 'dark'}; text contrast >= ${lowest.toFixed(2)}:1 (bg/surface/raised)\n`);
}

// tokens.css must carry exactly the derived default theme (no stale generated CSS).
const def = paletteById('obsidian');
const expected = buildTokens(def, fallbackAccent(deriveTheme(def)), 'solid', false);
for (const [k, v] of Object.entries(expected)) assert.ok(css.includes(`${k}: ${v};`), `tokens.css is stale: ${k} (run pnpm --dir packages/ui generate:themes)`);

if (failures.length) {
  process.stderr.write(`\n${failures.length} contrast violation(s):\n${failures.join('\n')}\n`);
  process.exit(1);
}
process.stdout.write(`\n${PALETTES.length} themes OK; accent / status / code / default-CSS checks passed\n`);
