// Architecture diagram. Every box maps to code in this repository:
//   daemon            apps/cli (`bot serve`) + crates/daemon-api (axum, tokio, SQLite)
//   Desktop app       apps/desktop (Tauri)             CLI          apps/cli (`bot`)
//   Telegram          crates/channel-telegram          node-agent   apps/node-agent (`bot-node`) + crates/peer-network (iroh)
//   permission gate   crates/permission-engine         secret store crates/secret-store
//   storage           crates/storage                   MCP          crates/mcp-bridge
//   connectors        crates/connector-email, -calendar, -homeassistant, crates/composio
//   providers         crates/provider-catalog, crates/cli-provider; docs/guide/providers-and-models.md
import { g, rect, text, caption, hair, tag, svgDoc, meshDefs, lucideIcon, brandIcon, path } from '../lib/core.mjs';
import { canvasBackground, glassCard, iconTile } from '../lib/bg.mjs';

const TITLE = 'Praxi architecture';
const DESC = 'The Praxi daemon (Rust, tokio, axum) listens on 127.0.0.1 only. The desktop app and CLI call its local API. '
  + 'The daemon runs the agent loop, checks every tool call in the permission engine and approval gate, and keeps secrets '
  + 'in the OS keychain. It dials out to LLM providers (cloud, or local Ollama and LM Studio), MCP servers, connectors and '
  + 'Telegram. Node-agents on other machines dial out to the daemon, so no inbound ports are needed.';

let uid = 0;

function arrow(x1, y1, x2, y2, T, { dash = false, color } = {}) {
  const c = color || T.muted;
  const dx = x2 - x1, dy = y2 - y1, len = Math.hypot(dx, dy), ux = dx / len, uy = dy / len;
  const hl = 8, hw = 4.6;
  const bx = x2 - ux * hl, by = y2 - uy * hl;
  return g({}, [
    tag('line', { x1, y1, x2: bx + ux * 1, y2: by + uy * 1, stroke: c, 'stroke-opacity': 0.7, 'stroke-width': 1.5,
      'stroke-dasharray': dash ? '4 4' : undefined, 'stroke-linecap': 'round' }),
    path({ d: 'M' + x2 + ' ' + y2 + 'L' + (bx - uy * hw) + ' ' + (by + ux * hw) + 'L' + (bx + uy * hw) + ' ' + (by - ux * hw) + 'Z',
      fill: c, 'fill-opacity': 0.85 }),
  ]);
}

// Content box. icon: { lucide } or { brand }; style 'row' (icon left) or 'col' (icon above).
function box({ x, y, w, h, title, sub = [], icon, T, defs, style = 'row', tsize = 17.5, ssize = 14, accent = false }) {
  const id = 'b' + (++uid);
  const card = glassCard('gc' + id, x, y, w, h, T, 16);
  const tile = iconTile('it' + id, 0, 0, 40, T);
  defs.push(card.defs, tile.defs);
  const ts = 40;
  let tx, ty, titleX, titleY, subY0;
  const lh = ssize * 1.38;
  if (style === 'row') {
    tx = x + 16; ty = y + (h - ts) / 2;
    titleX = tx + ts + 14;
    const block = tsize + (sub.length ? 6 + sub.length * lh : 0);
    const top = y + (h - block) / 2;
    titleY = top + tsize * 0.82; subY0 = top + tsize + 6 + ssize * 0.9;
  } else {
    tx = x + 16; ty = y + 16; titleX = x + 16; titleY = y + 16 + ts + 12 + tsize * 0.8; subY0 = titleY + 8 + ssize * 0.9;
  }
  const tileSvg = g({ transform: 'translate(' + tx + ' ' + ty + ')' }, [tile.svg]);
  const ic = icon.brand
    ? brandIcon(icon.brand, tx + 10, ty + 10, 20, T.brand)
    : lucideIcon(icon.lucide, tx + 10, ty + 10, 20, T.icon, 1.8);
  return g({}, [
    card.svg,
    accent ? rect({ x: x + 0.5, y: y + 0.5, width: w - 1, height: h - 1, rx: 15.5, fill: 'none', stroke: T.accentDeep, 'stroke-opacity': 0.5 }) : '',
    tileSvg, ic,
    text(titleX, titleY, title, tsize, T, { 'font-weight': 650, 'letter-spacing': -0.2 }),
    ...sub.map((ln, k) => text(titleX, subY0 + k * lh, ln, ssize, T, { fill: T.muted })),
  ]);
}

const label = (x, y, v, T, anchor) => caption(x, y, v, 12.5, T, { 'text-anchor': anchor, 'letter-spacing': 1.6, 'fill-opacity': 0.85 });
const edgeLabel = (x, y, v, T) => text(x, y, v, 12.5, T, { 'text-anchor': 'middle', fill: T.muted, 'fill-opacity': 0.9 });

function daemonCard(x, y, w, h, T, defs, mobile) {
  const card = glassCard('dc', x, y, w, h, T, 22);
  defs.push(card.defs);
  const ix = x + 24, iw = w - 48;
  const rows = mobile ? { agent: 100, gate: 200, tools: 336, store: 434, ph: 80 } : { agent: 96, gate: 194, tools: 324, store: 422, ph: 76 };
  const out = [
    card.svg,
    rect({ x: x + 0.5, y: y + 0.5, width: w - 1, height: h - 1, rx: 21.5, fill: 'none', stroke: T.accentDeep, 'stroke-opacity': T.name === 'dark' ? 0.45 : 0.5 }),
  ];
  // header
  const tile = iconTile('it-d', 0, 0, 44, T);
  defs.push(tile.defs);
  out.push(g({ transform: 'translate(' + ix + ' ' + (y + 22) + ')' }, tile.svg), lucideIcon('server', ix + 11, y + 33, 22, T.icon, 1.8));
  out.push(text(ix + 58, y + 42, 'Praxi daemon', mobile ? 24 : 22, T, { 'font-weight': 700, 'letter-spacing': -0.5 }));
  out.push(text(ix + 58, y + 62, 'bot serve · Rust · tokio · axum', mobile ? 15 : 13.5, T, { fill: T.muted }));
  const chipLabel = '127.0.0.1 only';
  if (!mobile) {
    const cw = 126, cx = x + w - 24 - cw;
    out.push(rect({ x: cx, y: y + 28, width: cw, height: 28, rx: 14, fill: T.accentDeep, 'fill-opacity': 0.14, stroke: T.accentDeep, 'stroke-opacity': 0.5 }));
    out.push(lucideIcon('lock', cx + 12, y + 35, 14, T.accent, 2));
    out.push(text(cx + 32, y + 46.5, chipLabel, 12.5, T, { fill: T.accent, 'font-weight': 650 }));
  }
  const sub = (l) => (mobile ? l.join(' ') : null);
  const bx = ix, bw = iw;
  const ly = y + 96;
  const inner = [];
  inner.push(box({ x: bx, y: y + rows.agent, w: bw, h: 76, icon: { lucide: 'bot' }, title: 'Agent loop', T, defs,
    sub: mobile ? ['Runs, subagents, workflows', 'and schedules'] : ['Runs, subagents, workflows and schedules'], tsize: mobile ? 19 : 17.5, ssize: mobile ? 15 : 14 }));
  inner.push(arrow(x + w / 2, y + rows.agent + 76 + 3, x + w / 2, y + rows.gate - 3, T, { color: T.accent }));
  // gate band
  const gh = mobile ? 114 : 108;
  inner.push(rect({ x: bx, y: y + rows.gate, width: bw, height: gh, rx: 16, fill: T.accentDeep, 'fill-opacity': T.name === 'dark' ? 0.17 : 0.14 }));
  inner.push(rect({ x: bx + 0.5, y: y + rows.gate + 0.5, width: bw - 1, height: gh - 1, rx: 15.5, fill: 'none', stroke: T.accentDeep, 'stroke-opacity': 0.7 }));
  inner.push(lucideIcon('shield-check', bx + 18, y + rows.gate + 18, 26, T.accent, 1.8));
  inner.push(text(bx + 60, y + rows.gate + 33, mobile ? 'Permission engine' : 'Permission engine and approval gate', mobile ? 19 : 17.5, T, { 'font-weight': 700, 'letter-spacing': -0.2 }));
  const gateSub = mobile
    ? ['Checked in code on every call.', 'Reads free, changes ask,', 'money always asks.']
    : ['Checked in code on every tool call.', 'Reads are free, changes ask, money', 'always asks. Decisions are audited.'];
  gateSub.forEach((ln, k) => inner.push(text(bx + 60, y + rows.gate + 56 + k * (mobile ? 21 : 19), ln, mobile ? 15 : 14, T, { fill: T.muted })));
  inner.push(arrow(x + w / 2, y + rows.gate + gh + 3, x + w / 2, y + rows.tools - 3, T, { color: T.accent }));
  inner.push(box({ x: bx, y: y + rows.tools, w: bw, h: 76, icon: { lucide: 'wrench' }, title: 'Tools', T, defs,
    sub: ['Files, shell, browser, desktop,', 'scripts, web, email'], tsize: mobile ? 19 : 17.5, ssize: mobile ? 15 : 14 }));
  const hw = (bw - 14) / 2;
  inner.push(box({ x: bx, y: y + rows.store, w: hw, h: rows.ph, icon: { lucide: 'lock-keyhole' }, title: 'Secret store', T, defs,
    sub: ['OS keychain or', 'encrypted file'], tsize: mobile ? 18 : 16.5, ssize: mobile ? 14.5 : 13.5 }));
  inner.push(box({ x: bx + hw + 14, y: y + rows.store, w: hw, h: rows.ph, icon: { lucide: 'database' }, title: 'Storage', T, defs,
    sub: ['SQLite, event log,', 'append-only audit'], tsize: mobile ? 18 : 16.5, ssize: mobile ? 14.5 : 13.5 }));
  return out.join('') + inner.join('');
}

export function architectureScene(T, mobile = false) {
  const defs = [meshDefs(T)];
  const out = [];
  let W, H, blobs;
  if (!mobile) {
    W = 1280; H = 740;
    const lx = 48, lw = 256, dx = 416, dw = 448, rx = 976, rw = 256, bh = 80;
    out.push(caption(48, 52, 'ARCHITECTURE', 15, T), caption(W - 48, 52, 'ALPHA · NODE-AGENTS ARE EXPERIMENTAL', 15, T, { 'text-anchor': 'end' }));
    out.push(hair(48, W - 48, 78, T));
    // column labels
    out.push(label(lx, 124, 'CLIENTS', T), label(lx, 350, 'CHANNEL', T), label(lx, 484, 'REMOTE MACHINES', T));
    out.push(label(rx, 124, 'MODELS', T), label(rx, 350, 'TOOLS AND APPS', T));
    out.push(daemonCard(dx, 96, dw, 526, T, defs, false));
    const left = [
      { y: 140, title: 'Desktop app', sub: ['Tauri shell, starts', 'the daemon for you'], icon: { brand: 'tauri' }, arrow: 'in', edge: 'local API' },
      { y: 234, title: 'bot CLI', sub: ['Chat, approve, configure', 'from a terminal'], icon: { lucide: 'terminal' }, arrow: 'in', edge: 'local API' },
      { y: 374, title: 'Telegram', sub: ['Paired accounts only,', 'inline approve buttons'], icon: { brand: 'telegram' }, arrow: 'out', edge: 'long polling' },
      { y: 508, title: 'Node-agents', sub: ['bot-node on your other', 'PCs, no inbound ports'], icon: { lucide: 'laptop' }, arrow: 'in', edge: 'iroh (QUIC)', h: 78 },
    ];
    for (const b of left) {
      out.push(box({ x: lx, y: b.y, w: lw, h: b.h || bh, title: b.title, sub: b.sub, icon: b.icon, T, defs }));
      const cy = b.y + (b.h || bh) / 2;
      if (b.arrow === 'in') out.push(arrow(lx + lw + 6, cy, dx - 4, cy, T)); else out.push(arrow(dx - 4, cy, lx + lw + 6, cy, T));
      out.push(edgeLabel((lx + lw + dx) / 2, cy - 9, b.edge, T));
    }
    const right = [
      { y: 140, title: 'Cloud providers', sub: ['Gemini, Anthropic,', 'OpenRouter, DeepSeek'], icon: { lucide: 'cloud' }, edge: 'HTTPS' },
      { y: 234, title: 'Local models', sub: ['Ollama, LM Studio,', 'any OpenAI-compatible'], icon: { brand: 'ollama' }, edge: 'localhost' },
      { y: 374, title: 'MCP servers', sub: ['Local programs (stdio)', 'or HTTP endpoints'], icon: { brand: 'modelcontextprotocol' }, edge: 'stdio / HTTP' },
      { y: 468, title: 'Connectors', sub: ['Email, calendar, Home', 'Assistant, Composio apps'], icon: { lucide: 'plug' }, edge: 'outbound' },
    ];
    for (const b of right) {
      out.push(box({ x: rx, y: b.y, w: rw, h: bh, title: b.title, sub: b.sub, icon: b.icon, T, defs }));
      const cy = b.y + bh / 2;
      out.push(arrow(dx + dw + 4, cy, rx - 6, cy, T));
      out.push(edgeLabel((dx + dw + rx) / 2, cy - 9, b.edge, T));
    }
    out.push(hair(48, W - 48, 650, T));
    out.push(caption(48, 678, 'ARROWS POINT FROM THE SIDE THAT OPENS THE CONNECTION', 13, T));
    out.push(caption(W - 48, 678, 'NOTHING LISTENS BEYOND 127.0.0.1', 13, T, { 'text-anchor': 'end' }));
    H = 704;
    blobs = [['glow', 640, 360, 520, 340, 0, 0.95], ['ember', 1150, 60, 320, 200, 20, 0.5], ['dusk', 120, 660, 420, 150, 0, 0.7], ['rose', 40, 40, 260, 130, 0, 0.3]];
  } else {
    W = 640; H = 1224;
    const px = 24, cw = W - 48;
    out.push(caption(px, 50, 'ARCHITECTURE', 15, T), caption(W - px, 50, 'ALPHA', 15, T, { 'text-anchor': 'end' }));
    out.push(hair(px, W - px, 72, T));
    out.push(label(px, 112, 'CLIENTS AND CHANNELS', T));
    const col3 = (cw - 24) / 3;
    const top = [
      { title: 'Desktop app', sub: ['Tauri shell'], icon: { brand: 'tauri' }, dir: 'down' },
      { title: 'bot CLI', sub: ['Terminal'], icon: { lucide: 'terminal' }, dir: 'down' },
      { title: 'Telegram', sub: ['Polled by', 'the daemon'], icon: { brand: 'telegram' }, dir: 'up' },
    ];
    const topY = 128, topH = 138;
    top.forEach((b, i) => {
      const x = px + i * (col3 + 12);
      out.push(box({ x, y: topY, w: col3, h: topH, title: b.title, sub: b.sub, icon: b.icon, T, defs, style: 'col', tsize: 18, ssize: 15 }));
      const cx = x + col3 / 2, y1 = topY + topH + 4, y2 = 322 - 4;
      out.push(b.dir === 'down' ? arrow(cx, y1, cx, y2, T) : arrow(cx, y2, cx, y1, T));
    });
    out.push(edgeLabel(219, 300, 'local API on 127.0.0.1', T));
    out.push(daemonCard(px, 326, cw, 536, T, defs, true));
    // below the daemon
    const by = 934, bh2 = 204;
    const bottom = [
      { title: 'Node-agents', sub: ['bot-node on', 'your other PCs.', 'No inbound ports.', 'Experimental.'], icon: { lucide: 'laptop' }, dir: 'up', edge: 'iroh' },
      { title: 'Models', sub: ['Cloud: Gemini,', 'Anthropic, OpenRouter,', 'DeepSeek. Local:', 'Ollama, LM Studio.'], icon: { lucide: 'cpu' }, dir: 'down', edge: 'HTTPS' },
      { title: 'MCP and apps', sub: ['MCP servers,', 'email, calendar,', 'Home Assistant,', 'Composio.'], icon: { brand: 'modelcontextprotocol' }, dir: 'down', edge: 'outbound' },
    ];
    bottom.forEach((b, i) => {
      const x = px + i * (col3 + 12);
      out.push(box({ x, y: by, w: col3, h: bh2, title: b.title, sub: b.sub, icon: b.icon, T, defs, style: 'col', tsize: 17, ssize: 14.5 }));
      const cx = x + col3 / 2;
      if (b.dir === 'up') out.push(arrow(cx, by - 4, cx, 862 + 4, T)); else out.push(arrow(cx, 862 + 4, cx, by - 4, T));
      out.push(text(cx + 12, 906, b.edge, 12.5, T, { fill: T.muted }));
    });
    H = 1224;
    out.push(hair(px, W - px, 1166, T));
    out.push(caption(px, 1190, 'ARROWS POINT FROM THE SIDE THAT OPENS', 13, T));
    out.push(caption(px, 1210, 'THE CONNECTION. NOTHING LISTENS BEYOND 127.0.0.1', 13, T));
    blobs = [['glow', 320, 600, 460, 420, 0, 0.9], ['ember', 600, 40, 260, 200, 20, 0.5], ['dusk', 60, 1150, 360, 150, 0, 0.7]];
  }
  const body = g({}, [canvasBackground(W, H, T, blobs, { floor: false }), out.join('')]);
  return svgDoc({ width: W, height: H, title: TITLE, desc: DESC, T, defs: defs.join(''), body });
}
