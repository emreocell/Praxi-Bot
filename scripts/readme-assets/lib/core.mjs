// Shared building blocks for the README artwork: SVG helpers, themes, the
// grainy mesh background, clay shapes and icon drawing. No dependencies.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { lucide, brands } from './icons-data.mjs';

const here = fileURLToPath(new URL('../', import.meta.url));
export const fontBase64 = readFileSync(here + 'fonts/manrope-latin.woff2').toString('base64');

export const escape = value => String(value).replace(/[&<>"']/g, ch => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;',
})[ch]);
const num = v => (typeof v === 'number' ? +v.toFixed(2) : v);
export const tag = (name, attrs = {}, content = null) => {
  const a = Object.entries(attrs).filter(([, v]) => v !== undefined && v !== null)
    .map(([k, v]) => ' ' + k + '="' + escape(num(v)) + '"').join('');
  return '<' + name + a + (content === null ? '/>' : '>' + content + '</' + name + '>');
};
export const g = (attrs, body) => tag('g', attrs, Array.isArray(body) ? body.join('') : body);
export const rect = a => tag('rect', a);
export const circle = a => tag('circle', a);
export const ellipse = a => tag('ellipse', a);
export const path = a => tag('path', a);
export const stop = (offset, color, opacity = 1) => tag('stop', { offset, 'stop-color': color, 'stop-opacity': opacity });
export const radial = (id, stops, extra = {}) => tag('radialGradient', { id, ...extra }, stops.map(s => stop(...s)).join(''));
export const linear = (id, stops, extra = {}) => tag('linearGradient', { id, ...extra }, stops.map(s => stop(...s)).join(''));

export const FONT = 'Manrope,Arial,sans-serif';

// ----------------------------------------------------------------- themes
// Praxi palette: Obsidian dark and Clay light, ember (terracotta) as the accent.
export const themes = {
  dark: {
    name: 'dark', ink: '#111217', text: '#F6F0E7', muted: '#A7A097', accent: '#EBA886', accentDeep: '#C86B4A',
    hair: '#FDEFD6', hairOpacity: 0.14, cardFill: '#FDEFD6', cardFillOpacity: 0.045, cardStroke: '#FDEFD6',
    cardStrokeOpacity: 0.11, tile: '#FDEFD6', tileOpacity: 0.07, icon: '#F6E3CC', shadow: '#000000',
    brand: '#CFC7BC',
    canvasStroke: '#FDEFD6', canvasStrokeOpacity: 0.1, grainSoft: 0.5, grainPlain: 0.07, grainMultiply: 0,
  },
  light: {
    name: 'light', ink: '#F4F1ED', text: '#1A1A1A', muted: '#6B645B', accent: '#8A4E2A', accentDeep: '#C86B4A',
    hair: '#1A1A1A', hairOpacity: 0.13, cardFill: '#FFFFFF', cardFillOpacity: 0.55, cardStroke: '#FFFFFF',
    cardStrokeOpacity: 0.9, tile: '#C86B4A', tileOpacity: 0.12, icon: '#8A4E2A', shadow: '#8A4E2A',
    brand: '#5E564D',
    canvasStroke: '#1A1A1A', canvasStrokeOpacity: 0.1, grainSoft: 0.55, grainPlain: 0.05, grainMultiply: 0.12,
  },
};

// ------------------------------------------------------------------- text
// Greedy word wrap by character count (Manrope is close enough to proportional for fixed layouts).
export const wrap = (value, max) => value.split(' ').reduce((acc, word) => {
  const last = acc[acc.length - 1];
  if (last !== undefined && (last + ' ' + word).length <= max) acc[acc.length - 1] = last + ' ' + word; else acc.push(word);
  return acc;
}, []);

export const text = (x, y, value, size, T, extra = {}) => tag('text', {
  x, y, 'font-size': size, 'font-family': FONT, fill: T.text, ...extra,
}, escape(value));
export const caption = (x, y, value, size, T, extra = {}) => text(x, y, value, size, T, {
  fill: T.muted, 'font-weight': 500, 'letter-spacing': +(size * 0.12).toFixed(2), ...extra,
});
export const hair = (x1, x2, y, T, opacity = T.hairOpacity) => tag('line', {
  x1, y1: y, x2, y2: y, stroke: T.hair, 'stroke-opacity': opacity,
});

// ------------------------------------------------------------------ icons
// Lucide icon (stroke, 24 grid) drawn at (x, y) top-left with the given pixel size.
export const lucideIcon = (name, x, y, size, color, strokeWidth = 1.75) => {
  if (!lucide[name]) throw new Error('Unknown lucide icon: ' + name);
  return g({
    transform: 'translate(' + num(x) + ' ' + num(y) + ') scale(' + num(size / 24) + ')', fill: 'none', stroke: color,
    'stroke-width': strokeWidth, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true',
  }, lucide[name]);
};
// Brand mark (simple-icons, filled, 24 grid).
export const brandIcon = (name, x, y, size, color) => {
  if (!brands[name]) throw new Error('Unknown brand icon: ' + name);
  return g({ transform: 'translate(' + num(x) + ' ' + num(y) + ') scale(' + num(size / 24) + ')', 'aria-hidden': 'true' },
    path({ d: brands[name], fill: color }));
};

// ----------------------------------------------------------- shared defs
export function baseDefs(width, height, T, radius = 24) {
  return tag('defs', {}, [
    tag('style', {}, '@font-face{font-family:Manrope;src:url(data:font/woff2;base64,' + fontBase64
      + ') format("woff2");font-style:normal;font-weight:400 800;}text{font-kerning:normal;}'),
    tag('filter', { id: 'blur', x: '-35%', y: '-35%', width: '170%', height: '170%', 'color-interpolation-filters': 'sRGB' },
      tag('feGaussianBlur', { stdDeviation: 24 })),
    tag('filter', { id: 'blur-shadow', x: '-60%', y: '-500%', width: '220%', height: '1100%', 'color-interpolation-filters': 'sRGB' },
      tag('feGaussianBlur', { stdDeviation: 22 })),
    tag('filter', { id: 'blur-sm', x: '-50%', y: '-50%', width: '200%', height: '200%', 'color-interpolation-filters': 'sRGB' },
      tag('feGaussianBlur', { stdDeviation: 9 })),
    tag('filter', { id: 'blur-xs', x: '-50%', y: '-50%', width: '200%', height: '200%', 'color-interpolation-filters': 'sRGB' },
      tag('feGaussianBlur', { stdDeviation: 3.5 })),
    tag('filter', { id: 'grain', x: '0%', y: '0%', width: '100%', height: '100%' }, [
      tag('feTurbulence', { type: 'fractalNoise', baseFrequency: '.44', numOctaves: 2, stitchTiles: 'stitch', seed: 17 }),
      tag('feColorMatrix', { type: 'saturate', values: 0 }),
      tag('feComponentTransfer', {}, tag('feFuncA', { type: 'linear', slope: 0.7 })),
    ].join('')),
    tag('clipPath', { id: 'canvas' }, rect({ width, height, rx: radius })),
  ].join(''));
}

// Grain overlays: soft-light for the mesh, a plain pass for texture on flat areas.
export const grain = (width, height, T, x = 0, y = 0, extraClip) => g({ 'clip-path': extraClip }, [
  rect({ x, y, width, height, filter: 'url(#grain)', opacity: T.grainSoft, style: 'mix-blend-mode:soft-light' }),
  rect({ x, y, width, height, filter: 'url(#grain)', opacity: T.grainPlain }),
  T.grainMultiply ? rect({ x, y, width, height, filter: 'url(#grain)', opacity: T.grainMultiply, style: 'mix-blend-mode:multiply' }) : '',
]);

// Mesh gradient stop sets per theme.
export function meshDefs(T) {
  if (T.name === 'dark') {
    return [
      radial('m-ember', [[0, '#FDEFD6'], ['26%', '#F0B992', 0.94], ['60%', '#C86B4A', 0.72], [1, '#5B2B1C', 0]]),
      radial('m-pearl', [[0, '#FFF8EC', 0.95], ['34%', '#F4D3B6', 0.82], ['72%', '#A2573A', 0.34], [1, '#3A1D15', 0]]),
      radial('m-rose', [[0, '#C9A39A', 0.5], ['60%', '#7A5A5C', 0.2], [1, T.ink, 0]]),
      radial('m-dusk', [[0, '#41506E', 0.55], ['60%', '#262E44', 0.24], [1, T.ink, 0]]),
      radial('m-glow', [[0, '#7A3B25', 0.62], ['55%', '#2B1A19', 0.34], [1, T.ink, 0]]),
      radial('m-floor', [[0, '#8A4E2A', 0.34], ['65%', '#3A2219', 0.16], [1, T.ink, 0]]),
    ].join('');
  }
  return [
    radial('m-ember', [[0, '#FFF3E2'], ['28%', '#F3C3A2', 0.95], ['62%', '#D9825F', 0.72], [1, '#C86B4A', 0]]),
    radial('m-pearl', [[0, '#FFFFFF', 0.95], ['34%', '#FDEFD6', 0.9], ['72%', '#E8B999', 0.4], [1, '#D8D3CC', 0]]),
    radial('m-rose', [[0, '#E9B7A0', 0.7], ['60%', '#E2C6B8', 0.3], [1, T.ink, 0]]),
    radial('m-dusk', [[0, '#D8D3CC', 0.95], ['60%', '#E2DDD6', 0.5], [1, T.ink, 0]]),
    radial('m-glow', [[0, '#F0C9AE', 0.7], ['55%', '#F4DCC8', 0.4], [1, T.ink, 0]]),
    radial('m-floor', [[0, '#C86B4A', 0.26], ['65%', '#D9A78C', 0.12], [1, T.ink, 0]]),
  ].join('');
}

// ---------------------------------------------------------- canvas frame
// Wraps scene content in the standard SVG shell with title/desc and a rounded canvas.
export function svgDoc({ width, height, title, desc, T, defs = '', body, radius = 24, frame = true }) {
  return '<?xml version="1.0" encoding="UTF-8"?>\n' + tag('svg', {
    xmlns: 'http://www.w3.org/2000/svg', width, height, viewBox: '0 0 ' + width + ' ' + height,
    role: 'img', 'aria-labelledby': 'title description',
  }, [
    tag('title', { id: 'title' }, escape(title)),
    tag('desc', { id: 'description' }, escape(desc)),
    baseDefs(width, height, T, radius), tag('defs', {}, defs),
    body,
    frame ? rect({ x: 0.5, y: 0.5, width: width - 1, height: height - 1, rx: radius - 0.5, fill: 'none',
      stroke: T.canvasStroke, 'stroke-opacity': T.canvasStrokeOpacity }) : '',
  ].join('\n')) + '\n';
}

// ----------------------------------------------------------- clay shapes
// Matte, eyeless clay forms (the look of Praxi bots): a soft radial body gradient,
// bounce light, a diffuse highlight and fine grain, with a contact shadow beneath.
export const clayTints = {
  cream: { hi: '#FFFCF6', base: '#EEE6DA', shade: '#BDB1A2', deep: '#7B7164', bounce: '#F2C9A8' },
  ember: { hi: '#F9C3A3', base: '#D9794F', shade: '#9C4A2D', deep: '#5E2A19', bounce: '#F0A57F' },
  amber: { hi: '#F7DDB2', base: '#DDB27B', shade: '#A5753F', deep: '#60401F', bounce: '#F1CC9B' },
  sand: { hi: '#FFFFFF', base: '#E4DCCF', shade: '#B1A594', deep: '#6E6457', bounce: '#F0CDB2' },
};
let clayCounter = 0;
export function clay({ kind, x, y, size, rotate = 0, tint = 'cream', T, shadow = 1, elongate = 1.0 }) {
  const id = 'c' + (++clayCounter);
  const C = clayTints[tint];
  const w = kind === 'capsule' ? size * 0.5 : size;
  const h = kind === 'capsule' ? size * elongate * 1.25 : size;
  const shapeAttrs = kind === 'sphere'
    ? { cx: 0, cy: 0, r: size / 2 }
    : { x: -w / 2, y: -h / 2, width: w, height: h, rx: kind === 'capsule' ? w / 2 : size * 0.3 };
  const shape = (extra = {}) => tag(kind === 'sphere' ? 'circle' : 'rect', { ...shapeAttrs, ...extra });
  const defs = [
    radial(id + 'b', [[0, C.hi], ['34%', C.base], ['78%', C.shade], [1, C.deep]], { cx: '36%', cy: '28%', r: '82%', fx: '36%', fy: '28%' }),
    radial(id + 'r', [[0, C.bounce, 0.55], [1, C.bounce, 0]], { cx: '60%', cy: '96%', r: '52%' }),
    tag('clipPath', { id: id + 'k' }, shape()),
    linear(id + 'l', [[0, '#FFFFFF', 0.55], ['45%', '#FFFFFF', 0], [1, C.hi, 0.25]], { x1: 0, y1: 0, x2: 1, y2: 1 }),
  ].join('');
  const shadowColor = T.shadow;
  const sOp = T.name === 'dark' ? 0.55 : 0.30;
  const contact = ellipse({
    cx: x + size * 0.06, cy: y + (kind === 'capsule' ? h : size) * 0.52 + size * 0.1,
    rx: (kind === 'capsule' ? w : size) * 0.46, ry: size * 0.13, fill: shadowColor, opacity: sOp * shadow, filter: 'url(#blur-shadow)',
  });
  const body = g({ transform: 'translate(' + num(x) + ' ' + num(y) + ') rotate(' + rotate + ')' }, [
    shape({ fill: 'url(#' + id + 'b)' }),
    g({ 'clip-path': 'url(#' + id + 'k)' }, [
      rect({ x: -w, y: -h, width: w * 2, height: h * 2, fill: 'url(#' + id + 'r)' }),
      ellipse({ cx: -w * 0.2, cy: -h * 0.27, rx: w * 0.3, ry: h * 0.2, fill: '#FFFFFF', opacity: tint === 'ember' ? 0.3 : 0.5,
        filter: 'url(#blur-sm)', transform: 'rotate(-18 ' + num(-w * 0.2) + ' ' + num(-h * 0.27) + ')' }),
      rect({ x: -w, y: -h, width: w * 2, height: h * 2, filter: 'url(#grain)', opacity: 0.5, style: 'mix-blend-mode:soft-light' }),
    ]),
    shape({ fill: 'none', stroke: 'url(#' + id + 'l)', 'stroke-width': 1.5 }),
  ]);
  return { defs, svg: contact + body };
}
