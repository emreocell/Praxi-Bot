// Independent check of the emitted surface tokens.
//
// It does NOT look at the resolver's plan.  It takes the CSS custom properties the app would set
// (`buildThemeTokens`), parses the emitted fills, filters, scrim and modal overlay, and composites
// them over fixture backdrops with its own colour maths: `saturate()` per the Filter Effects matrix,
// a direct separable Gaussian blur with mirrored edges at the emitted radius, source-over
// compositing in sRGB, WCAG contrast.  The only code shared with the app is the thing under test
// (the tone sampler + resolver + token builder) and the scrim mask geometry.
import { buildThemeTokens, PANEL_BLUR_MAX, PANEL_BLUR_MIN, PANEL_TRANSPARENCY_MAX, PANEL_TRANSPARENCY_MIN, TEXT_TARGET }
  from '../src/surfaceModel.ts';
import { evaluateTone, makeToneSource } from '../src/toneModel.ts';
import { DEFAULT_CUSTOM_THEME, PALETTES, UI_STYLES, deriveTheme, fallbackAccent } from '../src/themeModel.ts';
import { scrimCoverageX, textExtent } from '../../../apps/desktop/ui/src/lib/scrimMask.ts';

/* ---------- colour maths (own implementation) ---------- */
const lin = (v) => { const c = v / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
const lum = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
const contrast = (a, b) => { const x = lum(a); const y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
const hex = (h) => { const n = parseInt(h.slice(1, 7), 16); return [(n >> 16) & 255, (n >> 8) & 255, n & 255]; };
const clamp255 = (v) => Math.max(0, Math.min(255, v));

function saturate([r, g, b], s) {
  return [
    clamp255((0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b),
    clamp255((0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b),
    clamp255((0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b)
  ];
}

const reflect = (i, n) => { const p = 2 * n; const m = ((i % p) + p) % p; return m < n ? m : p - 1 - m; };

/** Direct separable Gaussian blur, kernel to 3 sigma, mirrored edges.  `rgb` is w*h*3. */
function gaussian(rgb, w, h, sigma) {
  const radius = Math.ceil(sigma * 3);
  const kernel = Array.from({ length: 2 * radius + 1 }, (_, i) => Math.exp(-((i - radius) ** 2) / (2 * sigma * sigma)));
  const total = kernel.reduce((a, b) => a + b, 0);
  const k = kernel.map((v) => v / total);
  const tmp = new Float64Array(rgb.length);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) for (let c = 0; c < 3; c++) {
    let acc = 0;
    for (let j = -radius; j <= radius; j++) acc += k[j + radius] * rgb[(y * w + reflect(x + j, w)) * 3 + c];
    tmp[(y * w + x) * 3 + c] = acc;
  }
  const out = new Float64Array(rgb.length);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) for (let c = 0; c < 3; c++) {
    let acc = 0;
    for (let j = -radius; j <= radius; j++) acc += k[j + radius] * tmp[(reflect(y + j, h) * w + x) * 3 + c];
    out[(y * w + x) * 3 + c] = acc;
  }
  return out;
}

/* ---------- parsing the emitted tokens ---------- */

/** Split `a, b(c, d), e` at top-level commas. */
function topLevel(s) {
  const parts = []; let depth = 0; let cur = '';
  for (const ch of s) {
    if (ch === '(') depth++;
    if (ch === ')') depth--;
    if (ch === ',' && depth === 0) { parts.push(cur.trim()); cur = ''; } else cur += ch;
  }
  parts.push(cur.trim());
  return parts;
}

/** A CSS colour (hex, hex8, transparent, var(), color-mix in srgb) -> { rgb, a }.  Premultiplied mixing like CSS. */
export function parseColor(str, tokens, depth = 0) {
  const s = String(str).trim();
  if (depth > 8) throw new Error(`colour too deep: ${s}`);
  if (s === 'transparent') return { rgb: [0, 0, 0], a: 0 };
  if (s.startsWith('var(')) {
    const name = s.slice(4, s.indexOf(')')).split(',')[0].trim();
    if (tokens[name] === undefined) throw new Error(`unknown token ${name}`);
    return parseColor(tokens[name], tokens, depth + 1);
  }
  if (s.startsWith('#')) {
    const a = s.length === 9 ? parseInt(s.slice(7, 9), 16) / 255 : 1;
    return { rgb: hex(s), a };
  }
  if (s.startsWith('color-mix(')) {
    const inner = s.slice('color-mix('.length, -1);
    const [space, first, second] = topLevel(inner);
    if (!/^in srgb$/i.test(space)) throw new Error(`unsupported colour space: ${space}`);
    const part = (p) => {
      const m = /^(.*?)\s+(\d+(?:\.\d+)?)%$/.exec(p);
      return m ? { c: parseColor(m[1], tokens, depth + 1), w: Number(m[2]) / 100 } : { c: parseColor(p, tokens, depth + 1), w: null };
    };
    let A = part(first); let B = part(second);
    if (A.w === null && B.w === null) { A.w = 0.5; B.w = 0.5; } else if (A.w === null) A.w = 1 - B.w; else if (B.w === null) B.w = 1 - A.w;
    const a = A.c.a * A.w + B.c.a * B.w;
    if (a === 0) return { rgb: [0, 0, 0], a: 0 };
    const rgb = [0, 1, 2].map((i) => (A.c.rgb[i] * A.c.a * A.w + B.c.rgb[i] * B.c.a * B.w) / a);
    return { rgb, a };
  }
  throw new Error(`cannot parse colour: ${s}`);
}

export function parseFilter(str) {
  const s = String(str).trim();
  if (s === 'none') return { blur: 0, sat: 1 };
  const blur = /blur\((\d+(?:\.\d+)?)px\)/.exec(s);
  const sat = /saturate\((\d+(?:\.\d+)?)\)/.exec(s);
  if (!blur && !sat) throw new Error(`cannot parse filter: ${s}`);
  return { blur: blur ? Number(blur[1]) : 0, sat: sat ? Number(sat[1]) : 1 };
}

const over = (below, { rgb, a }) => below.map((v, i) => v * (1 - a) + rgb[i] * a);

/* ---------- fixtures: the backdrops the guarantee is checked over ---------- */
const W = 72;
const H = 48;
function fixture(name, px) {
  const data = new Uint8ClampedArray(W * H * 4);
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) data.set([...px(x, y), 255], (y * W + x) * 4);
  return { name, data };
}
const flat = (c) => () => c;
const patch = (x0, y0, size, color, ground) => (x, y) => (x >= x0 && x < x0 + size && y >= y0 && y < y0 + size ? color : ground);
export const FIXTURES = [
  fixture('white', flat([255, 255, 255])),
  fixture('black', flat([0, 0, 0])),
  fixture('grey', flat([128, 128, 128])),
  fixture('orange', flat([217, 130, 43])),
  fixture('blue', flat([74, 123, 208])),
  fixture('magenta aa00aa', flat([170, 0, 170])),
  fixture('green | gray', (x) => (x < W / 2 ? [0, 255, 0] : [184, 184, 184])),
  fixture('white speck at corner on dark', patch(0, 0, 3, [255, 255, 255], [10, 10, 10])),
  fixture('white patch 16px centre on dark', patch(28, 16, 16, [255, 255, 255], [10, 10, 10])),
  fixture('dark patch at edge on light', patch(0, 20, 12, [0, 0, 0], [240, 240, 240])),
  fixture('dots', (x, y) => (x % 4 < 2 && y % 4 < 2 ? [255, 255, 255] : [16, 16, 16])),
  fixture('gradient', (x) => [Math.round((x / (W - 1)) * 255), Math.round((x / (W - 1)) * 200), 90])
];
const PANE_WIDTHS = [320, 760, 1400];
const CORE_THEMES = ['obsidian', 'cobalt', 'raven', 'clay'];

/* ---------- the check ---------- */
const sources = new Map();
const toneCache = new Map();
const fieldCache = new Map();
const hotCache = new Map();

function toneFor(fx, theme, dim) {
  const d = deriveTheme(theme);
  const key = `${fx.name}|${d.bg}|${dim}`;
  if (!toneCache.has(key)) {
    if (!sources.has(fx.name)) sources.set(fx.name, makeToneSource(fx.data, W, H, 1));
    toneCache.set(key, evaluateTone(sources.get(fx.name), { bg: d.bg, dim }));
  }
  return toneCache.get(key);
}

const blurCache = new Map();
/** Blur of the raw fixture (once per radius): the dim layers and the modal overlay are affine, so they commute with it. */
function blurredRaw(fx, sigma) {
  const key = `${fx.name}|${sigma}`;
  if (!blurCache.has(key)) {
    const raw = new Float64Array(W * H * 3);
    for (let i = 0; i < W * H; i++) for (let c = 0; c < 3; c++) raw[i * 3 + c] = fx.data[i * 4 + c];
    blurCache.set(key, sigma > 0 ? gaussian(raw, W, H, sigma) : raw);
  }
  return blurCache.get(key);
}

/**
 * saturate(blur(backdrop)) as a glass layer's fill sees it: the fixture under the theme's dim layer
 * (and a modal's overlay), blurred at the emitted radius, then saturated at the emitted strength.
 */
function field(fx, bg, dim, filter, overlay) {
  const key = `${fx.name}|${bg}|${dim}|${filter.blur}|${filter.sat}|${overlay ? JSON.stringify(overlay) : ''}`;
  if (fieldCache.has(key)) return fieldCache.get(key);
  const b = hex(bg);
  const blurred = blurredRaw(fx, filter.blur);
  const out = new Float64Array(W * H * 3);
  for (let i = 0; i < W * H; i++) {
    let c = [0, 1, 2].map((ch) => (1 - dim) * blurred[i * 3 + ch] + dim * b[ch]);
    if (overlay) c = over(c, overlay);
    if (filter.sat !== 1) c = saturate(c, filter.sat);
    out.set(c, i * 3);
  }
  fieldCache.set(key, out);
  return out;
}

/** The pixels worth compositing: extremes by luminance and by channel, and by luminance after mixing toward black / white. */
function hotPixels(rgb, key) {
  if (hotCache.has(key)) return hotCache.get(key);
  const picks = new Map();
  const top = (score, n) => {
    const best = [];
    for (let i = 0; i < W * H; i++) {
      const v = score(rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]);
      if (best.length < n || v > best[best.length - 1].v) {
        best.push({ v, i });
        best.sort((a, b) => b.v - a.v);
        if (best.length > n) best.pop();
      }
    }
    for (const { i } of best) picks.set(i, [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]]);
  };
  const mixTo = (r, g, b, t, a) => [r * (1 - a) + t * a, g * (1 - a) + t * a, b * (1 - a) + t * a];
  top((r, g, b) => lum([r, g, b]), 8);
  top((r, g, b) => -lum([r, g, b]), 8);
  for (const ch of [0, 1, 2]) {
    top((...c) => c[ch], 3);
    top((...c) => -c[ch], 3);
  }
  top((r, g, b) => lum(mixTo(r, g, b, 0, 0.6)), 4);
  top((r, g, b) => -lum(mixTo(r, g, b, 255, 0.6)), 4);
  const unique = [...picks.values()];
  hotCache.set(key, unique);
  return unique;
}

/**
 * Violations of the legibility guarantee for every theme x style x chat area x slider extreme x
 * backdrop, measured on the emitted tokens.  `mutate(tokens, ctx)` may corrupt them (self-test);
 * `coverage` is the scrim mask geometry; `subset` narrows the run for the self-test.
 */
export function checkSurfaces({ mutate, coverage = scrimCoverageX, subset } = {}) {
  const failures = [];
  let checks = 0;
  const themes = [...PALETTES, { id: 'custom (default)', ...DEFAULT_CUSTOM_THEME }].filter((t) => !subset?.themes || subset.themes.includes(t.id));
  const fixtures = FIXTURES.filter((f) => !subset?.fixtures || subset.fixtures.includes(f.name));
  for (const theme of themes) {
    const d = deriveTheme(theme);
    const accent = theme.accent ?? fallbackAccent(d);
    for (const style of UI_STYLES.filter((s) => !subset?.styles || subset.styles.includes(s))) {
      for (const transparentChat of [false, true]) {
        for (const transparency of [PANEL_TRANSPARENCY_MIN, PANEL_TRANSPARENCY_MAX]) {
          for (const blur of [PANEL_BLUR_MIN, PANEL_BLUR_MAX]) {
            for (const dim of CORE_THEMES.includes(theme.id) ? [0, 0.55] : [0]) {
              for (const fx of fixtures) {
                const tone = toneFor(fx, theme, dim);
                let t = buildThemeTokens(theme, accent, style, true, { transparency, blur, transparentChat, tone });
                if (mutate) t = mutate({ ...t }, { theme, style, transparentChat });
                const cell = `${theme.id}/${style}/${transparentChat ? 'clear' : 'pane'}/t${transparency}/b${blur}/dim${dim}/${fx.name}`;
                const fills = (...names) => names.map((n) => parseColor(t[n], t));
                const modal = parseColor(t['--color-modal-backdrop'], t);
                const accentFill = parseColor(`color-mix(in srgb, ${t['--color-accent']} 20%, var(--color-surface-elevated))`, t);
                const accentGradient = { rgb: hex(t['--color-accent']), a: 0.12 };
                // Scrim strength under the narrowest coverage of the reading column at each pane width.
                const minCoverage = Math.min(...PANE_WIDTHS.map((w) => {
                  const [l, r] = textExtent(w);
                  let m = 1; for (let x = Math.ceil(l); x <= r; x += 4) m = Math.min(m, coverage(w, x));
                  return m;
                }));
                const scrim = parseColor(t['--center-scrim'], t);
                const scrimEff = { rgb: scrim.rgb, a: scrim.a * minCoverage };
                /** [name, filter token or 'none', pre-overlay, fills over it, target for primary text, primary text only] */
                const stacks = [
                  ['shell panel', t['--shell-filter'], null, fills('--shell-fill'), TEXT_TARGET],
                  ['card in panel', t['--shell-filter'], null, fills('--shell-fill', '--color-surface'), 4.5],
                  ['raised card in panel', t['--shell-filter'], null, fills('--shell-fill', '--color-surface-elevated'), 4.5],
                  ['own bubble in panel', t['--shell-filter'], null, [...fills('--shell-fill'), accentFill, accentGradient], 4.5, true],
                  ['popover / menu / toast', t['--float-filter'], null, fills('--float-fill'), 4.5],
                  ['modal dialog', t['--float-filter'], modal, fills('--float-fill'), 4.5]
                ];
                if (transparentChat) {
                  stacks.push(
                    ['chat scrim', 'none', null, [scrimEff], TEXT_TARGET],
                    ['bubble on scrim', 'none', null, [scrimEff, ...fills('--color-surface-elevated')], 4.5],
                    ['own bubble on scrim', 'none', null, [scrimEff, accentFill, accentGradient], 4.5, true],
                    ['composer', t['--composer-filter'], null, fills('--composer-fill'), 4.5]
                  );
                } else {
                  stacks.push(['center pane', t['--center-filter'], null, fills('--center-fill'), TEXT_TARGET]);
                }
                const texts = [
                  ['text-secondary', t['--color-text-secondary'], 4.5], ['text-muted', t['--color-text-muted'], 4.5],
                  ['success', t['--color-success'], 4.5], ['warning', t['--color-warning'], 4.5], ['danger', t['--color-danger'], 4.5],
                  ['accent-text', t['--color-accent-text'], 4.5], ['text-faint', t['--color-text-faint'], 3], ['control border', t['--color-border-strong'], 3]
                ];
                for (const [name, filterStr, overlay, chain, target, primaryOnly] of stacks) {
                  const filter = parseFilter(filterStr);
                  const key = `${fx.name}|${d.bg}|${dim}|${filter.blur}|${filter.sat}|${overlay ? 'm' + t['--color-modal-backdrop'] : ''}`;
                  const rgb = field(fx, d.bg, dim, filter, overlay);
                  for (const px of hotPixels(rgb, key)) {
                    const composite = chain.reduce((c, f) => over(c, f), px);
                    // Only the message text lives inside the user's own bubble (no status / muted text there).
                    // Faint text and control borders are unconstrained on Solid panels (never were); they are
                    // re-derived (and checked) wherever the panel is translucent.
                    const applicable = (primaryOnly ? [] : texts).filter(([label]) => style !== 'solid' || !/faint|control border/.test(label));
                    for (const [label, color, min] of [['text', t['--color-text'], target], ...applicable]) {
                      checks++;
                      const c = contrast(hex(color), composite);
                      if (c < min) failures.push(`${cell}: ${label} on ${name} ${c.toFixed(2)} < ${min}`);
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
  return { failures, checks };
}

/** Corruptions of the emitted tokens (and of the scrim mask) that the check must catch. */
export const MUTATIONS = [
  { name: 'shell fill transparent (the review\'s mutation)', mutate: (t) => ({ ...t, '--shell-fill': 'transparent', '--center-fill': 'transparent' }), subset: { themes: ['obsidian', 'clay'], styles: ['glass', 'tinted'], fixtures: ['white', 'black', 'dots'] } },
  { name: 'opaque-chat centre fill transparent', mutate: (t, c) => (c.transparentChat ? t : { ...t, '--center-fill': 'transparent' }), subset: { themes: ['obsidian'], styles: ['glass'], fixtures: ['white'] } },
  { name: 'scrim transparent over a clear chat area', mutate: (t) => ({ ...t, '--center-scrim': 'transparent' }), subset: { themes: ['obsidian', 'clay'], styles: ['glass'], fixtures: ['white', 'black'] } },
  { name: 'floating fill transparent', mutate: (t) => ({ ...t, '--float-fill': 'transparent' }), subset: { themes: ['obsidian', 'clay'], styles: ['glass'], fixtures: ['white', 'black'] } },
  { name: 'composer fill transparent', mutate: (t) => ({ ...t, '--composer-fill': 'transparent' }), subset: { themes: ['obsidian'], styles: ['glass'], fixtures: ['white'] } },
  { name: 'emitted saturate() stronger than the maths assumed', mutate: (t) => Object.fromEntries(Object.entries(t).map(([k, v]) => [k, typeof v === 'string' ? v.replace(/saturate\([\d.]+\)/g, 'saturate(4)') : v])), subset: { themes: ['obsidian'], styles: ['glass'], fixtures: ['magenta aa00aa'] } },
  { name: 'emitted blur weaker than the maths assumed', mutate: (t) => Object.fromEntries(Object.entries(t).map(([k, v]) => [k, typeof v === 'string' ? v.replace(/blur\([\d.]+px\)/g, 'blur(1px)') : v])), subset: { themes: ['obsidian'], styles: ['glass'], fixtures: ['white patch 16px centre on dark', 'dark patch at edge on light', 'white speck at corner on dark'] } },
  {
    name: 'scrim mask feathered inside the text column (the pre-fix geometry)',
    mutate: (t) => t,
    coverage: (paneW, x) => {
      const half = Math.min(paneW / 2 - 64, 400);
      const dist = Math.abs(x - paneW / 2);
      return dist <= half ? 1 : Math.max(0, 1 - (dist - half) / 64);
    },
    subset: { themes: ['obsidian'], styles: ['glass'], fixtures: ['white'] }
  }
];

/** Every mutation must produce violations.  Returns the names of those that slipped through. */
export function selfTest() {
  const escaped = [];
  for (const m of MUTATIONS) {
    const { failures } = checkSurfaces({ mutate: m.mutate, coverage: m.coverage, subset: m.subset });
    if (!failures.length) escaped.push(m.name);
  }
  return escaped;
}
