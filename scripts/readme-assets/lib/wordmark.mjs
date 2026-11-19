// The Praxi wordmark: Manrope Bold with explicit glyph positions (identical in every
// renderer) and a dotless i topped with a small clay sphere. Advances are the
// shaped Manrope wght=700 values in em, measured once with HarfBuzz.
import { g, tag, radial, circle, ellipse, FONT } from './core.mjs';

const advances = [0, 0.6385, 1.025, 1.601, 2.155]; // P r a x i (dotless)
const stemCenter = 0.1402; // centre of the dotless i stem, em from its origin
const dotRadius = 0.108;
const dotCenterY = 0.7; // em above the baseline

export const wordmarkWidth = (size, tracking = -0.03) => size * (2.155 + 0.2805 + tracking * 4);

export function wordmark(x, baseline, size, T, { tracking = -0.03 } = {}) {
  const xs = advances.map((a, i) => (x + (a + tracking * i) * size).toFixed(2)).join(' ');
  const dotX = x + (advances[4] + tracking * 4 + stemCenter) * size;
  const dotY = baseline - dotCenterY * size;
  const r = dotRadius * size;
  const defs = radial('wm-dot', [[0, '#FBD2B6'], ['40%', '#E58A5E'], ['78%', '#B65A38'], [1, '#7A3A22']],
    { cx: '36%', cy: '30%', r: '78%', fx: '36%', fy: '30%' });
  const svg = g({}, [
    tag('text', {
      x: xs, y: baseline, 'font-size': size, 'font-family': FONT, 'font-weight': 700, fill: T.text,
    }, 'Praxı'),
    circle({ cx: dotX, cy: dotY, r, fill: 'url(#wm-dot)' }),
    ellipse({ cx: dotX - r * 0.3, cy: dotY - r * 0.42, rx: r * 0.42, ry: r * 0.26, fill: '#FFFFFF', opacity: 0.32, filter: 'url(#blur-xs)',
      transform: 'rotate(-22 ' + (dotX - r * 0.32).toFixed(2) + ' ' + (dotY - r * 0.4).toFixed(2) + ')' }),
  ]);
  return { defs, svg };
}
