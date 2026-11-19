import { g, rect, ellipse, path, tag, caption, text, hair, grain, meshDefs, svgDoc } from '../lib/core.mjs';
import { wordmark } from '../lib/wordmark.mjs';
import { character } from '../lib/characters.mjs';

const TITLE = 'Praxi: a local-first, open-source AI agent platform';
const DESC = 'Praxi-Bot is a self-hosted, open-source AI agent that works on your own computer under permission rules '
  + 'you set. Local-first, built in Rust.';

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

export function hero(T, mobile = false) {
  const W = mobile ? 640 : 1280, H = mobile ? 790 : 540;
  const inset = mobile ? 40 : 72;
  // The real bot characters (see lib/characters.mjs): ghost in white at the front, others in app tints.
  const shapes = mobile ? [
    character({ shape: 'cloud', tint: 'lilac', x: 134, y: 632, size: 176, rotate: -6, T }),
    character({ shape: 'capsule', tint: 'teal', x: 506, y: 592, size: 196, rotate: 12, T }),
    character({ shape: 'ghost', tint: 'white', x: 318, y: 610, size: 238, T }),
    character({ shape: 'pebble', tint: 'amber', x: 498, y: 660, size: 110, T }),
  ] : [
    character({ shape: 'cloud', tint: 'lilac', x: 912, y: 240, size: 184, rotate: -7, T }),
    character({ shape: 'capsule', tint: 'teal', x: 1160, y: 262, size: 208, rotate: 13, T }),
    character({ shape: 'ghost', tint: 'white', x: 1036, y: 318, size: 262, T }),
    character({ shape: 'pebble', tint: 'amber', x: 1166, y: 398, size: 128, T }),
  ];
  const wm = wordmark(inset - (mobile ? 2 : 4), mobile ? 262 : 268, mobile ? 188 : 196, T);
  const defs = meshDefs(T) + wm.defs + shapes.map(c => c.defs).join('')
    + tag('linearGradient', { id: 'shade' }, stops([[0, T.ink], ['40%', T.ink, 0.88], ['72%', T.ink, 0.2], [1, T.ink, 0]]))
    + tag('linearGradient', { id: 'shade-v', x1: 0, y1: 0, x2: 0, y2: 1 },
      stops([[0, T.ink], ['48%', T.ink, 0.9], ['66%', T.ink, 0.25], [1, T.ink, 0]]));

  const body = g({ 'clip-path': 'url(#canvas)' }, [
    rect({ width: W, height: H, fill: T.ink }),
    mesh(T, mobile),
    mobile
      ? rect({ width: W, height: H, fill: 'url(#shade-v)', opacity: 0.85 })
      : rect({ width: W, height: H, fill: 'url(#shade)', opacity: 0.82 }),
    ellipse({ cx: W * 0.88, cy: H + 50, rx: W * 0.6, ry: 300, fill: 'url(#m-floor)' }),
    grain(W, H, T),
    shapes.map(c => c.svg).join(''),
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
    hair(inset, W - inset, 736, T),
    caption(inset, 766, 'ALPHA · APACHE-2.0', 15, T),
    caption(W - inset, 766, 'RUST · TAURI', 15, T, { 'text-anchor': 'end' }),
  ].join('');
}
