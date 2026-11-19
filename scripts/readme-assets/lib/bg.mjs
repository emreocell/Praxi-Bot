// Reusable canvas background (ink + blurred mesh blobs + grain) and glass cards.
import { g, rect, ellipse, tag, linear, grain, meshDefs, svgDoc } from './core.mjs';

// blobs: [gradientName, cx, cy, rx, ry, rotate, opacity]
export function canvasBackground(W, H, T, blobs, { floor = true } = {}) {
  return g({ 'clip-path': 'url(#canvas)' }, [
    rect({ width: W, height: H, fill: T.ink }),
    g({ filter: 'url(#blur)' }, blobs.map(([name, cx, cy, rx, ry, rot = 0, opacity = 1]) => ellipse({
      cx, cy, rx, ry, transform: rot ? 'rotate(' + rot + ' ' + cx + ' ' + cy + ')' : undefined,
      fill: name === 'ink' ? T.ink : 'url(#m-' + name + ')', opacity,
    }))),
    floor ? ellipse({ cx: W * 0.86, cy: H + 40, rx: W * 0.55, ry: 220, fill: 'url(#m-floor)', opacity: 0.8 }) : '',
    grain(W, H, T),
  ]);
}

// Frosted card: translucent fill, hairline border and a faint top-left sheen.
export function glassCard(id, x, y, w, h, T, radius = 20) {
  const defs = linear(id, [[0, '#FFFFFF', T.name === 'dark' ? 0.07 : 0.5], ['55%', '#FFFFFF', 0], [1, '#FFFFFF', 0]],
    { x1: 0, y1: 0, x2: 1, y2: 1 });
  const svg = g({}, [
    rect({ x, y, width: w, height: h, rx: radius, fill: T.cardFill, 'fill-opacity': T.cardFillOpacity }),
    rect({ x, y, width: w, height: h, rx: radius, fill: 'url(#' + id + ')' }),
    rect({ x: x + 0.5, y: y + 0.5, width: w - 1, height: h - 1, rx: radius - 0.5, fill: 'none',
      stroke: T.cardStroke, 'stroke-opacity': T.cardStrokeOpacity }),
  ]);
  return { defs, svg };
}

// Rounded icon tile with an ember wash.
export function iconTile(id, x, y, size, T) {
  const defs = linear(id, T.name === 'dark'
    ? [[0, '#E58A5E', 0.30], [1, '#8A4E2A', 0.12]] : [[0, '#E58A5E', 0.30], [1, '#C86B4A', 0.10]],
  { x1: 0, y1: 0, x2: 1, y2: 1 });
  const svg = g({}, [
    rect({ x, y, width: size, height: size, rx: size * 0.3, fill: 'url(#' + id + ')' }),
    rect({ x: x + 0.5, y: y + 0.5, width: size - 1, height: size - 1, rx: size * 0.3 - 0.5, fill: 'none',
      stroke: T.name === 'dark' ? '#F0B992' : '#C86B4A', 'stroke-opacity': T.name === 'dark' ? 0.28 : 0.35 }),
  ]);
  return { defs, svg };
}

// Small ember clay bead (timeline markers, divider dot).
export function bead(id, cx, cy, r) {
  const stops = [['0', '#FBD2B6'], ['40%', '#E58A5E'], ['78%', '#B65A38'], ['1', '#7A3A22']];
  const defs = tag('radialGradient', { id, cx: '36%', cy: '30%', r: '78%', fx: '36%', fy: '30%' },
    stops.map(s => tag('stop', { offset: s[0], 'stop-color': s[1] })).join(''));
  return { defs, svg: tag('circle', { cx, cy, r, fill: 'url(#' + id + ')' }) };
}

export { svgDoc, tag };
