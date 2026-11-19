import { g, rect, text, meshDefs, svgDoc, lucideIcon, wrap } from '../lib/core.mjs';
import { canvasBackground, glassCard, iconTile } from '../lib/bg.mjs';

// Every line is checked against docs/guide (see scripts/readme-assets/README.md).
export const features = [
  { icon: 'mouse-pointer-click', title: 'Desktop control',
    lines: ['Clicks, types and reads apps on', 'Windows and Linux (X11), and', 'drives Chromium via DevTools.'] },
  { icon: 'workflow', title: 'Multi-agent workflows',
    lines: ['Many bots, one profile each;', 'subagents handle sub-tasks and', 'workflows replay with no model.'] },
  { icon: 'shield-check', title: 'Permission engine',
    lines: ['Enforced in code, not by the', 'model. Changes ask first, money', 'always asks, all audited.'] },
  { icon: 'cpu', title: 'Local or cloud models',
    lines: ['Gemini, Anthropic, OpenRouter,', 'DeepSeek, Ollama, LM Studio or', 'any OpenAI-compatible server.'] },
  { icon: 'blocks', title: 'MCP, apps and skills',
    lines: ['MCP servers, email, calendar,', 'Home Assistant, Composio apps', 'and skills for tricky sites.'] },
  { icon: 'network', title: 'Node-agents', tag: 'EXPERIMENTAL',
    lines: ['Control another PC with no', 'inbound ports: the node dials', 'out over iroh (QUIC).'] },
  { icon: 'calendar-clock', title: 'Schedules',
    lines: ['Say when in plain English or', 'Turkish: "every weekday at 9",', '"in 2 hours", "her gün 09:00".'] },
  { icon: 'send', title: 'Telegram',
    lines: ['Chat from your phone and', 'approve actions with inline', 'buttons. Paired accounts only.'] },
];
for (const f of features) for (const ln of f.lines) {
  if (ln.length > 32) throw new Error('Feature line too long for the card: ' + ln);
}

const TITLE = 'What Praxi does';
const DESC = 'Eight capabilities: ' + features.map(f => f.title).join(', ') + '.';

export function featuresScene(T, mobile = false) {
  const defsParts = [meshDefs(T)];
  const cards = [];
  let W, H;
  if (!mobile) {
    W = 1280;
    const pad = 40, gap = 20, cw = (W - pad * 2 - gap * 3) / 4, ch = 232;
    H = pad * 2 + ch * 2 + gap;
    features.forEach((f, i) => {
      const x = pad + (i % 4) * (cw + gap), y = pad + Math.floor(i / 4) * (ch + gap);
      const card = glassCard('card' + i, x, y, cw, ch, T), tile = iconTile('tile' + i, x + 24, y + 24, 52, T);
      defsParts.push(card.defs, tile.defs);
      cards.push(card.svg, tile.svg, lucideIcon(f.icon, x + 24 + 13, y + 24 + 13, 26, T.icon, 1.7));
      cards.push(text(x + 24, y + 110, f.title, 21, T, { 'font-weight': 650, 'letter-spacing': -0.4 }));
      f.lines.forEach((ln, k) => cards.push(text(x + 24, y + 144 + k * 23, ln, 14.5, T, { fill: T.muted })));
      if (f.tag) cards.push(chip(x + cw - 24, y + 36, f.tag, T));
    });
  } else {
    W = 640;
    const pad = 24, gap = 14, cw = W - pad * 2, ch = 148;
    H = pad * 2 + ch * 8 + gap * 7;
    features.forEach((f, i) => {
      const x = pad, y = pad + i * (ch + gap);
      const card = glassCard('card' + i, x, y, cw, ch, T), tile = iconTile('tile' + i, x + 24, y + 24, 60, T);
      defsParts.push(card.defs, tile.defs);
      cards.push(card.svg, tile.svg, lucideIcon(f.icon, x + 24 + 15, y + 24 + 15, 30, T.icon, 1.7));
      cards.push(text(x + 108, y + 52, f.title, 26, T, { 'font-weight': 650, 'letter-spacing': -0.5 }));
      wrap(f.lines.join(' '), 48).forEach((ln, k) => cards.push(text(x + 108, y + 84 + k * 25, ln, 17.5, T, { fill: T.muted })));
      if (f.tag) cards.push(chip(x + cw - 22, y + 36, f.tag, T, 12));
    });
  }
  const blobs = !mobile
    ? [['glow', 1100, 80, 600, 320, 0, 1], ['ember', 1250, 10, 320, 190, 20, 0.75], ['glow', 140, 520, 420, 190, 0, 0.8], ['dusk', 560, 540, 460, 150, 0, 0.7], ['rose', 40, 40, 260, 140, 0, 0.35]]
    : [['glow', 600, 200, 460, 320, 0, 0.8], ['ember', 660, 40, 240, 200, 20, 0.5], ['dusk', 40, H - 120, 400, 200, 0, 0.7], ['glow', 560, H - 260, 380, 280, 0, 0.55]];
  const body = g({}, [canvasBackground(W, H, T, blobs), cards.join('')]);
  return svgDoc({ width: W, height: H, title: TITLE, desc: DESC, T, defs: defsParts.join(''), body });
}

function chip(xRight, yMid, label, T, size = 10.5) {
  const w = label.length * size * 0.74 + 20, h = size + 14;
  return g({}, [
    rect({ x: xRight - w, y: yMid - h / 2, width: w, height: h, rx: h / 2, fill: 'none', stroke: T.accent, 'stroke-opacity': 0.55 }),
    text(xRight - w / 2, yMid + size * 0.36, label, size, T, { fill: T.accent, 'font-weight': 700, 'letter-spacing': size * 0.12, 'text-anchor': 'middle' }),
  ]);
}
