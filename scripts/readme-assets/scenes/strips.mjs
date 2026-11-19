import { g, rect, text, caption, hair, svgDoc, meshDefs, tag, wrap } from '../lib/core.mjs';
import { canvasBackground, bead } from '../lib/bg.mjs';

export const steps = [
  { n: '1', cap: 'INSTALL', title: 'Install and pick a model',
    lines: ['Run the Windows installer and follow the', 'setup wizard. Pick a provider (Gemini has a', 'free tier); the key stays in your keychain.'] },
  { n: '2', cap: 'CREATE', title: 'Create a bot',
    lines: ['Name it, pick its models and a permission', 'preset. By default reads are free and every', 'change asks first.'] },
  { n: '3', cap: 'RUN', title: 'Chat, approve, schedule',
    lines: ['Watch it work in the app or on Telegram,', 'approve or deny each action, then put the', 'job on a schedule.'] },
];

export function stepsScene(T, mobile = false) {
  const defs = [meshDefs(T)];
  const out = [];
  let W, H;
  if (!mobile) {
    W = 1280; H = 300;
    const inset = 72, col = (W - inset * 2) / 3;
    steps.forEach((s, i) => {
      const x = inset + i * col;
      const b = bead('bead' + i, x + 8, 70, 8);
      defs.push(b.defs);
      out.push(b.svg);
      if (i < 2) out.push(hair(x + 30, x + col - 22, 70, T, 0.22));
      out.push(caption(x, 118, 'STEP ' + s.n + ' · ' + s.cap, 14, T));
      out.push(text(x - 1, 164, s.title, 29, T, { 'font-weight': 650, 'letter-spacing': -0.8 }));
      s.lines.forEach((ln, k) => out.push(text(x, 204 + k * 26, ln, 16.5, T, { fill: T.muted })));
    });
  } else {
    W = 640; H = 3 * 214 + 60;
    const inset = 40;
    steps.forEach((s, i) => {
      const y = 50 + i * 214;
      const b = bead('bead' + i, inset + 8, y + 12, 8);
      defs.push(b.defs);
      out.push(b.svg);
      if (i < 2) out.push(tag('line', { x1: inset + 8, y1: y + 36, x2: inset + 8, y2: y + 214 - 10, stroke: T.hair, 'stroke-opacity': 0.22 }));
      out.push(caption(inset + 36, y + 17, 'STEP ' + s.n + ' · ' + s.cap, 15, T));
      out.push(text(inset + 35, y + 58, s.title, 31, T, { 'font-weight': 650, 'letter-spacing': -0.8 }));
      wrap(s.lines.join(' '), 44).forEach((ln, k) => out.push(text(inset + 36, y + 96 + k * 28, ln, 19, T, { fill: T.muted })));
    });
  }
  const blobs = !mobile
    ? [['glow', 1180, 330, 520, 200, 0, 0.8], ['dusk', 100, -10, 360, 110, 0, 0.6], ['ember', 1280, 300, 260, 140, 0, 0.5]]
    : [['glow', 640, H, 420, 260, 0, 0.8], ['dusk', 0, 0, 300, 160, 0, 0.6]];
  return svgDoc({
    width: W, height: H, T, defs: defs.join(''),
    title: 'How Praxi works in three steps',
    desc: 'Step 1: ' + steps[0].title + '. Step 2: ' + steps[1].title + '. Step 3: ' + steps[2].title + '.',
    body: g({}, [canvasBackground(W, H, T, blobs, { floor: false }), out.join('')]),
  });
}

export const facts = [
  { v: '127.0.0.1', l: ['The daemon listens on', 'loopback only'] },
  { v: '0', l: ['inbound ports to open,', 'even on a remote host'] },
  { v: '0', l: ['telemetry, by design', '(ADR-0020)'] },
  { v: 'Apache-2.0', l: ['Open source, free to', 'use and build on'] },
];

export function statsScene(T, mobile = false) {
  const out = [];
  let W, H;
  if (!mobile) {
    W = 1280; H = 190;
    const inset = 72, col = (W - inset * 2) / 4;
    facts.forEach((f, i) => {
      const x = inset + i * col;
      if (i) out.push(tag('line', { x1: x - 24, y1: 40, x2: x - 24, y2: H - 40, stroke: T.hair, 'stroke-opacity': 0.14 }));
      out.push(text(x - 1, 94, f.v, f.v.length > 6 ? 36 : 52, T, { 'font-weight': 650, 'letter-spacing': -1.2 }));
      f.l.forEach((ln, k) => out.push(text(x, 128 + k * 22, ln, 15.5, T, { fill: T.muted })));
    });
  } else {
    W = 640; H = 340;
    const inset = 40, col = (W - inset * 2) / 2;
    facts.forEach((f, i) => {
      const x = inset + (i % 2) * col, y = 30 + Math.floor(i / 2) * 150;
      if (i % 2) out.push(tag('line', { x1: x - 18, y1: y + 10, x2: x - 18, y2: y + 120, stroke: T.hair, 'stroke-opacity': 0.14 }));
      out.push(text(x - 1, y + 62, f.v, f.v.length > 6 ? 36 : 54, T, { 'font-weight': 650, 'letter-spacing': -1.2 }));
      f.l.forEach((ln, k) => out.push(text(x, y + 96 + k * 24, ln, 17, T, { fill: T.muted })));
    });
    out.push(tag('line', { x1: inset, y1: 170, x2: W - inset, y2: 170, stroke: T.hair, 'stroke-opacity': 0.14 }));
  }
  const blobs = !mobile
    ? [['glow', 1100, 100, 500, 130, 0, 0.7], ['dusk', 160, 190, 360, 90, 0, 0.6]]
    : [['glow', 560, 340, 400, 200, 0, 0.7], ['dusk', 80, 0, 300, 120, 0, 0.5]];
  return svgDoc({
    width: W, height: H, T, defs: meshDefs(T),
    title: 'Praxi in four facts',
    desc: facts.map(f => f.v + ': ' + f.l.join(' ')).join('. ') + '.',
    body: g({}, [canvasBackground(W, H, T, blobs, { floor: false }), out.join('')]),
  });
}

export function dividerScene(T) {
  const W = 1280, H = 48, mid = W / 2;
  const b = bead('bead-d', mid, H / 2, 5);
  const accent = T.name === 'dark' ? '#E58A5E' : '#C86B4A';
  const grad = (id, x1, x2) => tag('linearGradient', { id, x1, y1: 0, x2, y2: 0 }, [
    ['0', T.hair, 0], ['0.5', accent, 0.55], ['1', T.hair, 0],
  ].map(s => tag('stop', { offset: s[0], 'stop-color': s[1], 'stop-opacity': s[2] })).join(''));
  const body = g({}, [
    rect({ x: 40, y: H / 2 - 0.5, width: mid - 40 - 22, height: 1, fill: 'url(#dv)' }),
    rect({ x: mid + 22, y: H / 2 - 0.5, width: mid - 40 - 22, height: 1, fill: 'url(#dv)' }),
    b.svg,
  ]);
  return svgDoc({
    width: W, height: H, T, defs: grad('dv', 0, 1) + b.defs, body, frame: false,
    title: 'Section divider', desc: 'A thin ember line with a small clay bead in the middle.',
  });
}
