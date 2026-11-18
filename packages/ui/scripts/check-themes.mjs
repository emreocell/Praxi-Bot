import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { URL } from 'node:url';
import process from 'node:process';
import {
  PALETTES, FAVORITE_THEMES, DEFAULT_CUSTOM_THEME, UI_STYLES, buildTokens, contrastRatio, deriveTheme,
  fallbackAccent, HIGH_CONTRAST_THEMES, inkOn, mix, normalizeHex, paletteById
} from '../src/themeModel.ts';
import { buildThemeTokens, effectiveUiStyle, TEXT_TARGET } from '../src/surfaceModel.ts';
import { checkSurfaces, FIXTURES, MUTATIONS, selfTest as selfTestSurfaces } from './surfaceCheck.mjs';

// Every shipped theme must be readable.  The derivation pulls faded text, status colours and
// accent text toward the text colour until they pass, so the hard requirement is the one
// the derivation cannot repair: the theme's own text colour on bg / surface / raised.
const root = new URL('../', import.meta.url);
const css = readFileSync(new URL('tokens.css', root), 'utf8');
assert.equal(PALETTES.length, 31, 'the prototype ships 31 themes (29 + 2 high contrast)');
assert.equal(new Set(PALETTES.map((p) => p.id)).size, PALETTES.length, 'theme ids are unique');
for (const id of FAVORITE_THEMES) assert.ok(paletteById(id), `favourite ${id} exists`);

// High-contrast themes (issue #238): AAA text, 4.5 / 3 accent and status, 3:1 focus and borders.
assert.deepEqual([...HIGH_CONTRAST_THEMES], ['contrast-dark', 'contrast-light'], 'both high-contrast themes are listed');
const hcRows = [];
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
  if (HIGH_CONTRAST_THEMES.includes(theme.id)) {
    const accent = theme.accent;
    assert.ok(accent, `${name}: has its own accent`);
    const grounds = [d.bg, d.surface, d.raised, d.hover];
    const hcMin = (label, fg, min, list = grounds) => {
      const worst = Math.min(...list.map((s) => check(name, `HC ${label}`, fg, s, min)));
      hcRows.push(`${name}	${label}	${fg}	${worst.toFixed(2)}	>= ${min}`);
    };
    hcMin('text', d.text, 7);
    hcMin('secondary text', d.secondary, 7);
    hcMin('muted text', d.muted, 4.5);
    hcMin('accent (text)', accent, 4.5);
    hcMin('accent (component)', accent, 3);
    for (const k of ['success', 'warning', 'danger']) { hcMin(`${k} (text)`, d[k], 4.5); hcMin(`${k} (component)`, d[k], 3); }
    hcMin('focus ring', d.focus, 4.5);
    hcMin('control border', d.lineStrong, 4.5);
    check(name, 'HC accent button ink', inkOn(accent), accent, 7);
    for (const key of ['--shell-fill', '--center-fill', '--float-fill', '--composer-fill']) {
      const v = buildTokens(theme, accent, 'solid', false)[key];
      assert.ok(v === undefined || !/rgba|transparent|blur/i.test(v), `${name}: ${key} is opaque`);
    }
  }
  check(name, 'danger button', d.dangerInk, d.danger, 4.5);
  for (const status of ['success', 'warning', 'danger']) {
    check(name, `${status} badge`, d[status], mix(d[status], d.surface, 0.9), 4.5);
  }
  process.stdout.write(`${name}: ${d.light ? 'light' : 'dark'}; text contrast >= ${lowest.toFixed(2)}:1 (bg/surface/raised)\n`);
}

if (process.env.HC_SUMMARY) for (const row of hcRows) process.stdout.write(`${row}
`);

// Translucent surfaces (card 244-E): the EMITTED tokens (fills, filters, scrim, modal overlay) are
// composited over fixture backdrops by an independent CSS emulator (scripts/surfaceCheck.mjs: its own
// saturate() matrix, Gaussian blur with mirrored edges, source-over compositing, WCAG contrast), for
// every theme x style x chat area x transparency/blur extreme x dim x backdrop.  Primary text must reach
// 7:1 on panels / scrim / center pane and 4.5:1 on cards, bubbles, composer and floating surfaces;
// muted / secondary / status / accent text 4.5:1; faint text and control borders 3:1.
const surfaces = checkSurfaces();
const byKind = new Map();
for (const f of surfaces.failures) {
  const m = /^(\S+?)\/(\S+?)\/(\w+)\/.*?: (.+?) on (.+?) [\d.]+ < /.exec(f);
  const k = m ? `${m[2]}/${m[3]}: ${m[4]} on ${m[5]}` : f;
  byKind.set(k, (byKind.get(k) ?? 0) + 1);
}
if (process.env.SURFACE_SUMMARY) for (const [k, n] of [...byKind].sort((a, b) => b[1] - a[1]).slice(0, 40)) process.stdout.write(`${n}	${k}
`);
for (const f of surfaces.failures.slice(0, 60)) failures.push(f);
if (surfaces.failures.length > 60) failures.push(`... and ${surfaces.failures.length - 60} more surface violations`);
const escaped = selfTestSurfaces();
for (const name of escaped) failures.push(`surface check self-test: mutation not caught: ${name}`);
for (const key of ['--shell-fill', '--shell-filter', '--center-fill', '--center-scrim', '--float-fill', '--composer-fill']) {
  assert.ok(buildThemeTokens(paletteById('obsidian'), '#b6b8fa', 'glass', true)[key], `token ${key} is emitted`);
}
// Platform fallbacks: reduced transparency paints solid; no backdrop-filter turns glass into tinted.
assert.equal(effectiveUiStyle('glass', { backdropFilter: true, reducedTransparency: true }), 'solid');
assert.equal(effectiveUiStyle('line', { backdropFilter: true, reducedTransparency: true }), 'line');
assert.equal(effectiveUiStyle('glass', { backdropFilter: false, reducedTransparency: false }), 'tinted');
assert.equal(effectiveUiStyle('tinted', { backdropFilter: false, reducedTransparency: false }), 'tinted');
process.stdout.write(`surfaces: ${surfaces.checks} composited checks on the emitted tokens (${FIXTURES.length} backdrops; text >= ${TEXT_TARGET}:1 on panels); self-test: ${MUTATIONS.length} corruptions all caught
`);

// tokens.css must carry exactly the derived default theme (no stale generated CSS).
const def = paletteById('obsidian');
const expected = buildThemeTokens(def, fallbackAccent(deriveTheme(def)), 'solid', false);
for (const [k, v] of Object.entries(expected)) assert.ok(css.includes(`${k}: ${v};`), `tokens.css is stale: ${k} (run pnpm --dir packages/ui generate:themes)`);

if (failures.length) {
  process.stderr.write(`\n${failures.length} contrast violation(s):\n${failures.join('\n')}\n`);
  process.exit(1);
}
process.stdout.write(`\n${PALETTES.length} themes OK; accent / status / code / default-CSS checks passed\n`);
