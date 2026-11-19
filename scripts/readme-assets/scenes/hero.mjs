import { g, rect, ellipse, path, tag, caption, text, hair, grain, meshDefs, svgDoc, radial } from '../lib/core.mjs';
import { wordmark } from '../lib/wordmark.mjs';
import { character, baseCss, reducedMotionCss } from '../lib/characters.mjs';

const TITLE = 'Praxi: a local-first, open-source AI agent platform';
const DESC = 'Praxi-Bot is a self-hosted, open-source AI agent that works on your own computer under permission rules '
  + 'you set. Local-first, built in Rust. A small cast of clay bot characters, with a ghost asking to Allow once or Deny.';

const stops = list => list.map(s => tag('stop', { offset: s[0], 'stop-color': s[1], 'stop-opacity': s[2] ?? 1 })).join('');

function mesh(T, mobile) {
  const transform = mobile ? 'translate(-390 330) scale(0.95)' : '';
  return g({ transform }, [
    ellipse({ cx: 990, cy: 280, rx: 600, ry: 420, fill: 'url(#m-glow)' }),
    g({ filter: 'url(#blur)' }, [
      ellipse({ cx: 1085, cy: 290, rx: 360, ry: 330, transform: 'rotate(30 1085 290)', fill: 'url(#m-ember)' }),
      ellipse({ cx: 1115, cy: 215, rx: 170, ry: 250, transform: 'rotate(34 1115 215)', fill: 'url(#m-pearl)', opacity: 0.8 }),
      ellipse({ cx: 905, cy: 120, rx: 280, ry: 150, transform: 'rotate(-20 905 120)', fill: 'url(#m-rose)' }),
      ellipse({ cx: 800, cy: 500, rx: 380, ry: 150, fill: 'url(#m-dusk)', opacity: 0.85 }),
      ellipse({ cx: 1000, cy: 610, rx: 330, ry: 90, transform: 'rotate(-10 1000 610)', fill: T.ink, opacity: 0.9 }),
      ellipse({ cx: 930, cy: 440, rx: 150, ry: 90, fill: 'url(#m-pearl)', opacity: 0.55 }),
    ]),
  ]);
}

// Seeded PRNG so the dust is identical on every build.
const prng = seed => () => {
  seed |= 0; seed = seed + 0x6D2B79F5 | 0;
  let t = Math.imul(seed ^ seed >>> 15, 1 | seed);
  t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t;
  return ((t ^ t >>> 14) >>> 0) / 4294967296;
};

// Tiny floating dust and sparkle grains (transform and opacity animation only).
function dust(box, count, T, seed) {
  const rnd = prng(seed);
  const [x0, y0, x1, y1] = box;
  const dark = T.name === 'dark';
  let out = '';
  for (let n = 0; n < count; n++) {
    const o = +(0.22 + rnd() * 0.5).toFixed(2), r = +(0.7 + rnd() * 1.3).toFixed(2);
    const cx = +(x0 + rnd() * (x1 - x0)).toFixed(1), cy = +(y0 + rnd() * (y1 - y0)).toFixed(1);
    const drift = 'dr' + (1 + (n % 3)), dd = (7 + rnd() * 5).toFixed(1), td = (3 + rnd() * 3.5).toFixed(1);
    out += tag('g', { style: 'animation:' + drift + ' ' + dd + 's ease-in-out -' + (rnd() * 8).toFixed(1) + 's infinite' },
      tag('circle', { cx, cy, r, fill: dark ? '#FFE4CB' : '#FFFFFF', opacity: o,
        style: '--o:' + o + ';animation:tw ' + td + 's ease-in-out -' + (rnd() * 4).toFixed(1) + 's infinite' }));
  }
  return out;
}

// A tiny approval card in the app's own words (Allow once / Deny), tied to the ghost by a small tail.
function approval(cx, cy, T, cls) {
  const w = 216, h = 52, x = cx - w / 2, y = cy - h / 2, dark = T.name === 'dark';
  const allowFill = dark ? '#EBA886' : T.accent, allowText = dark ? '#24110A' : '#FDEFD6';
  const panel = dark ? '#1A1013' : '#FFFFFF', panelOpacity = dark ? 0.66 : 0.82;
  return g({ class: cls }, [
    rect({ x, y, width: w, height: h, rx: 26, fill: panel, 'fill-opacity': panelOpacity }),
    path({ d: 'M' + (x + 38) + ' ' + (y + h - 1) + 'L' + (x + 58) + ' ' + (y + h - 1) + 'L' + (x + 44) + ' ' + (y + h + 13) + 'Z', fill: panel, 'fill-opacity': panelOpacity }),
    rect({ x: x + 0.5, y: y + 0.5, width: w - 1, height: h - 1, rx: 25.5, fill: 'none', stroke: dark ? '#FDEFD6' : '#FFFFFF', 'stroke-opacity': dark ? 0.2 : 0.95 }),
    rect({ x: x + 8, y: y + 10, width: 112, height: 32, rx: 16, fill: allowFill }),
    text(x + 64, y + 31, 'Allow once', 14.5, T, { 'text-anchor': 'middle', 'font-weight': 700, fill: allowText }),
    rect({ x: x + 128.5, y: y + 10.5, width: 79, height: 31, rx: 15.5, fill: 'none', stroke: T.text, 'stroke-opacity': 0.28 }),
    text(x + 168, y + 31, 'Deny', 14.5, T, { 'text-anchor': 'middle', 'font-weight': 600, 'fill-opacity': 0.9 }),
  ]);
}

// Back row first. x is the centre, base is the floor contact line, size the square body image.
const castDesktop = [
  { shape: 'cloud', tint: 'lilac', x: 900, base: 386, size: 152, rotate: 4, depth: 0.6, eyes: 'happy', blush: true,
    anim: { float: [6.4, 4, -2.1], breathe: [4.6, -1.2] } },
  { shape: 'capsule', tint: 'teal', x: 1170, base: 390, size: 178, rotate: -9, depth: 0.6, eyes: 'wink', look: [-0.9, 0.6],
    anim: { float: [5.2, 5, -0.8], breathe: [4.1, -2.4], blink: [5.4, -1.6] } },
  { shape: 'ghost', tint: 'white', x: 1042, base: 440, size: 262, rotate: 1.5, look: [0.9, -0.7],
    anim: { float: [5.8, 6, -1.7], breathe: [3.8, -0.6], blink: [4.4, -2.2], glance: [8.2, -3.1, 1.7, 0.4] } },
  { shape: 'flower', tint: 'orange', x: 924, base: 455, size: 114, rotate: 5, look: [1.1, -0.8], blush: true,
    anim: { float: [4.7, 4, -3.2], breathe: [3.5, -1.1], blink: [3.9, -0.9], glance: [6.6, -1.4, 1.4, -0.4] } },
  { shape: 'pebble', tint: 'amber', x: 1168, base: 459, size: 124, rotate: -5, eyes: 'wide', look: [-1.5, -0.2], blush: true,
    anim: { float: [4.3, 3, -1.1], breathe: [3.3, -2.0], blink: [4.8, -2.9], glance: [7.4, -4.2, -1.4, -0.4] } },
];
const castMobile = [
  { ...castDesktop[0], x: 122, base: 716, size: 140 },
  { ...castDesktop[1], x: 514, base: 722, size: 172 },
  { ...castDesktop[2], x: 322, base: 738, size: 236 },
  { ...castDesktop[3], x: 196, base: 756, size: 104 },
  { ...castDesktop[4], x: 440, base: 758, size: 108 },
];

export function hero(T, mobile = false) {
  const W = mobile ? 640 : 1280, H = mobile ? 830 : 540;
  const inset = mobile ? 40 : 72;
  const dark = T.name === 'dark';
  const glow = mobile ? [641, 605] : [1085, 290]; // centre of the ember glow in the mesh
  const chars = (mobile ? castMobile : castDesktop).map((c, i) => character(i, { ...c, glow }, T));
  const wm = wordmark(inset - (mobile ? 2 : 4), mobile ? 262 : 268, mobile ? 188 : 196, T);
  const floorGrad = radial('floor-g', dark
    ? [[0, '#F0B992', 0.24], [0.6, '#F0B992', 0.08], [1, '#F0B992', 0]]
    : [[0, '#FFFFFF', 0.8], [0.6, '#FFFFFF', 0.35], [1, '#FFFFFF', 0]]);
  const shadowGrad = radial('shadow', dark
    ? [[0, '#1A0803', 0.8], [0.55, '#2A0F06', 0.4], [1, '#2A0F06', 0]]
    : [[0, '#8A4E2A', 0.55], [0.55, '#B8714A', 0.24], [1, '#B8714A', 0]]);
  const blushGrad = radial('blush', [[0, '#FF6B7D', dark ? 0.55 : 0.5], [1, '#FF6B7D', 0]]);
  const css = baseCss + chars.map(c => c.css).join('')
    + '.bub{animation:bob-b 6.2s ease-in-out -2.6s infinite}@keyframes bob-b{0%,100%{transform:translateY(0)}50%{transform:translateY(-4px)}}'
    + reducedMotionCss;
  const defs = meshDefs(T) + wm.defs + chars.map(c => c.defs).join('') + floorGrad + shadowGrad + blushGrad + tag('style', {}, css)
    + tag('linearGradient', { id: 'shade' }, stops([[0, T.ink], ['40%', T.ink, 0.88], ['72%', T.ink, 0.2], [1, T.ink, 0]]))
    + tag('linearGradient', { id: 'shade-v', x1: 0, y1: 0, x2: 0, y2: 1 },
      stops([[0, T.ink], ['48%', T.ink, 0.9], ['66%', T.ink, 0.25], [1, T.ink, 0]]));

  const floor = mobile
    ? ellipse({ cx: 330, cy: 752, rx: 330, ry: 46, fill: 'url(#floor-g)' })
    : ellipse({ cx: 1040, cy: 447, rx: 330, ry: 46, fill: 'url(#floor-g)' });
  const bubble = mobile ? approval(506, 532, T, 'bub') : approval(1098, 150, T, 'bub');
  const dustBox = mobile ? [330, 490, 626, 770] : [840, 100, 1250, 470];

  const body = g({ 'clip-path': 'url(#canvas)' }, [
    rect({ width: W, height: H, fill: T.ink }),
    mesh(T, mobile),
    mobile
      ? rect({ width: W, height: H, fill: 'url(#shade-v)', opacity: 0.85 })
      : rect({ width: W, height: H, fill: 'url(#shade)', opacity: 0.82 }),
    ellipse({ cx: W * 0.88, cy: H + 50, rx: W * 0.6, ry: 300, fill: 'url(#m-floor)' }),
    grain(W, H, T),
    floor,
    chars.map(c => c.shadow).join(''),
    chars.map(c => c.body).join(''),
    bubble,
    dust(dustBox, mobile ? 16 : 20, T, mobile ? 7 : 11),
    wm.svg,
    mobile ? mobileText(T, inset, W) : desktopText(T, inset, W),
  ]);
  return svgDoc({ width: W, height: H, title: TITLE, desc: DESC, T, defs, body });
}

function desktopText(T, inset, W) {
  return [
    caption(inset, 66, 'LOCAL-FIRST · OPEN SOURCE · RUST', 17, T),
    caption(W - inset, 66, 'ALPHA · APACHE-2.0', 17, T, { 'text-anchor': 'end', fill: T.text, 'fill-opacity': 0.72 }),
    text(inset, 348, 'An agent that uses your computer.', 41, T, { 'font-weight': 500, 'letter-spacing': -1.2 }),
    text(inset, 398, 'Only as far as you allow.', 41, T, { fill: T.accent, 'font-weight': 500, 'letter-spacing': -1.2 }),
    text(inset, 452, 'Self-hosted. Any model, cloud or local. No telemetry.', 21, T, { fill: T.muted }),
    hair(inset, W - inset, 478, T),
    caption(inset, 512, 'RUST · TOKIO · AXUM · TAURI', 15, T),
    caption(W - inset, 512, 'DESKTOP APP · CLI · TELEGRAM', 15, T, { 'text-anchor': 'end', fill: T.text, 'fill-opacity': 0.72 }),
  ].join('');
}

function mobileText(T, inset, W) {
  return [
    caption(inset, 62, 'LOCAL-FIRST · OPEN SOURCE · RUST', 17, T),
    text(inset, 342, 'An agent that uses', 40, T, { 'font-weight': 500, 'letter-spacing': -1.2 }),
    text(inset, 390, 'your computer.', 40, T, { 'font-weight': 500, 'letter-spacing': -1.2 }),
    text(inset, 438, 'Only as far as you allow.', 40, T, { fill: T.accent, 'font-weight': 500, 'letter-spacing': -1.2 }),
    text(inset, 486, 'Self-hosted. Any model. No telemetry.', 22, T, { fill: T.muted }),
    hair(inset, W - inset, 776, T),
    caption(inset, 806, 'ALPHA · APACHE-2.0', 15, T),
    caption(W - inset, 806, 'RUST · TAURI', 15, T, { 'text-anchor': 'end' }),
  ].join('');
}
