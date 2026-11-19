// The real Praxi bot characters, drawn the way the desktop app draws them
// (packages/ui/src/components/BotAvatar.tsx): the off-white 3D clay body WebP is
// tinted with the app's gradient-map SVG filter (port of gradientMapConfig in
// packages/ui/src/botShape.ts) and two tilted dark pill eyes are laid on top
// using the per-shape eye coordinates from the same file.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { g, tag, rect, ellipse } from './core.mjs';

const ui = fileURLToPath(new URL('../../../packages/ui/src/', import.meta.url));

// Eye coordinates (64x64 viewBox) parsed from the app source so they never drift.
const eyes = Object.fromEntries([...readFileSync(ui + 'botShape.ts', 'utf8').matchAll(
  /id: '(\w+)', alias: '\w+', leftEyeX: ([\d.]+), rightEyeX: ([\d.]+), eyeY: ([\d.]+)/g,
)].map(m => [m[1], { l: +m[2], r: +m[3], y: +m[4] }]));

const webp = {};
const bodyData = shape => webp[shape] ??= 'data:image/webp;base64,'
  + readFileSync(ui + 'assets/chars/' + shape + '-lg.webp').toString('base64');

// App tint swatches (CHARACTER_COLORS in botShape.ts) and the eye colour token.
export const tints = {
  white: '#f4f4f5', clay: '#b98a5e', red: '#f2545b', orange: '#f5803e', amber: '#f2b233',
  green: '#34c77b', teal: '#2fc4b2', blue: '#4c8dff', lilac: '#9b7bff', pink: '#f25fa8', grey: '#8e9099',
};
const EYE_COLOR = '#172125'; // --color-creature-eyes

const hex = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
const toHex = (r, gg, b) => '#' + [r, gg, b].map(n => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, '0')).join('');
const mix = (a, b, t) => { const [ar, ag, ab] = hex(a), [br, bg, bb] = hex(b); return toHex(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t); };

function gradientMap(color) {
  const [r, gg, b] = hex(color);
  const sat = toHex(r * r / 255, gg * gg / 255, b * b / 255);
  const stops = [mix(sat, '#000000', 0.38), mix(color, sat, 0.8), mix(color, sat, 0.38), color, mix(color, '#ffffff', 0.42)];
  const table = i => stops.map(s => (hex(s)[i] / 255).toFixed(3)).join(' ');
  return { r: table(0), g: table(1), b: table(2) };
}

let n = 0;
// size = display size in px of the (square) body image; rotate in degrees around its centre.
export function character({ shape, tint, x, y, size, rotate = 0, T, shadow = 1 }) {
  const id = 'ch' + (++n);
  const gm = gradientMap(tints[tint] || tint);
  const e = eyes[shape];
  if (!e) throw new Error('Unknown character shape: ' + shape);
  const fn = (name, tableValues) => tag(name, { type: 'table', tableValues });
  const defs = tag('filter', { id, x: 0, y: 0, width: 1, height: 1, 'color-interpolation-filters': 'sRGB' }, [
    tag('feColorMatrix', { type: 'matrix', values: '.3 .59 .11 0 0 .3 .59 .11 0 0 .3 .59 .11 0 0 0 0 0 1 0' }),
    tag('feComponentTransfer', {}, ['feFuncR', 'feFuncG', 'feFuncB'].map(f => tag(f, { type: 'linear', slope: 2.38, intercept: -1.31 })).join('')),
    tag('feComponentTransfer', {}, fn('feFuncR', gm.r) + fn('feFuncG', gm.g) + fn('feFuncB', gm.b)),
  ].join(''));
  const k = size / 64;
  const pill = cx => g({ transform: 'rotate(9 ' + cx + ' ' + e.y + ')' },
    rect({ x: cx - 2.8, y: e.y - 5.3, width: 5.6, height: 10.6, rx: 2.8, fill: EYE_COLOR }));
  const dark = T.name === 'dark';
  const contact = ellipse({
    cx: x + size * 0.02, cy: y + size * 0.465, rx: size * 0.38, ry: size * 0.085,
    fill: T.shadow, opacity: (dark ? 0.75 : 0.42) * shadow, filter: 'url(#blur-shadow)',
  });
  const body = g({ transform: 'translate(' + x + ' ' + y + ') rotate(' + rotate + ')' }, [
    tag('image', { href: bodyData(shape), x: -size / 2, y: -size / 2, width: size, height: size, filter: 'url(#' + id + ')' }),
    g({ transform: 'translate(' + (-size / 2) + ' ' + (-size / 2) + ') scale(' + k + ')' }, pill(e.l) + pill(e.r)),
  ]);
  return { defs, svg: contact + body };
}
