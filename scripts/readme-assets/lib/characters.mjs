// The real Praxi bot characters, drawn the way the desktop app draws them
// (packages/ui/src/components/BotAvatar.tsx): the off-white 3D clay body WebP is
// tinted with the app's gradient-map SVG filter (port of gradientMapConfig in
// packages/ui/src/botShape.ts) and dark pill eyes are laid on top using the
// per-shape eye coordinates from the same file.
//
// On top of the app look this module adds what a still hero needs: rim light,
// colour bleed from the mesh glow, haze for characters in the back, a coloured
// contact shadow, expressions (pills, happy arcs, wink, wide eyes, blush) and
// CSS animations. Only transform and opacity are animated; every animated
// element is a plain group, never a filtered one.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { g, tag, rect, ellipse, circle, path, radial } from './core.mjs';

const ui = fileURLToPath(new URL('../../../packages/ui/src/', import.meta.url));

// Eye coordinates (64x64 viewBox) parsed from the app source so they never drift.
const eyes = Object.fromEntries([...readFileSync(ui + 'botShape.ts', 'utf8').matchAll(
  /id: '(\w+)', alias: '\w+', leftEyeX: ([\d.]+), rightEyeX: ([\d.]+), eyeY: ([\d.]+)/g,
)].map(m => [m[1], { l: +m[2], r: +m[3], y: +m[4] }]));

const bodyData = shape => 'data:image/webp;base64,'
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

const f2 = v => +v.toFixed(2);

/**
 * One character. Returns { defs, css, shadow, body } so the caller can paint all
 * shadows first (on the floor) and then the bodies from back to front.
 *
 *  shape, tint     art and app tint swatch
 *  x, base, size   horizontal centre, floor contact y and display size of the square body image
 *  rotate          lean in degrees (positive leans right)
 *  depth           0 = front, 1 = far back (smaller is set by the caller; this adds haze and a touch of blur)
 *  glow            [x, y] of the mesh glow: rim light and colour bleed face it
 *  eyes            'pill' | 'happy' | 'wink' | 'wide'; look = [dx, dy] in the 64-unit eye grid
 *  blush           cheek colour spots
 *  anim            { float: [seconds, amplitude px, delay], breathe: [s, delay], blink: [s, delay], glance: [s, delay, dx, dy] }
 */
export function character(i, o, T) {
  const { shape, tint, x, base, size, rotate = 0, depth = 0, glow, look = [0, 0], blush = false, anim } = o;
  const kind = o.eyes || 'pill';
  const e = eyes[shape];
  if (!e) throw new Error('Unknown character shape: ' + shape);
  const dark = T.name === 'dark';
  const y = base - size * 0.44; // centre of the body image
  const sign = glow[0] >= x ? 1 : -1; // which side the glow is on
  const d = f2(Math.max(1.6, size * 0.011));
  const gm = gradientMap(tints[tint] || tint);
  const tr = (name, tableValues) => tag(name, { type: 'table', tableValues });
  const rim = dark ? '#FFD9B5' : '#FFFFFF';
  const fid = 'cf' + i, mid = 'cm' + i, iid = 'ci' + i, bid = 'cb' + i;

  const filter = tag('filter', { id: fid, x: 0, y: 0, width: 1, height: 1, 'color-interpolation-filters': 'sRGB' }, [
    tag('feColorMatrix', { type: 'matrix', values: '.3 .59 .11 0 0 .3 .59 .11 0 0 .3 .59 .11 0 0 0 0 0 1 0' }),
    tag('feComponentTransfer', {}, ['feFuncR', 'feFuncG', 'feFuncB'].map(f => tag(f, { type: 'linear', slope: 2.38, intercept: -1.31 })).join('')),
    tag('feComponentTransfer', { result: 'tint' }, tr('feFuncR', gm.r) + tr('feFuncG', gm.g) + tr('feFuncB', gm.b)),
    depth ? tag('feGaussianBlur', { in: 'tint', stdDeviation: f2(0.3 + depth * 0.45), result: 'tint' }) : '',
    // rim light on the edges that face the glow
    tag('feOffset', { in: 'SourceAlpha', dx: -sign * d, dy: d, result: 'o' }),
    tag('feComposite', { in: 'SourceAlpha', in2: 'o', operator: 'out', result: 'edge' }),
    tag('feGaussianBlur', { in: 'edge', stdDeviation: f2(d * 0.35), result: 'edgeb' }),
    tag('feFlood', { 'flood-color': rim, 'flood-opacity': dark ? 0.5 : 0.85, result: 'fl' }),
    tag('feComposite', { in: 'fl', in2: 'edgeb', operator: 'in', result: 'rim' }),
    tag('feMerge', {}, tag('feMergeNode', { in: 'tint' }) + tag('feMergeNode', { in: 'rim' })),
  ].join(''));

  const half = size / 2;
  const image = tag('image', { id: iid, href: bodyData(shape), x: -half, y: -half, width: size, height: size });
  const mask = tag('mask', { id: mid, maskUnits: 'userSpaceOnUse', x: -half, y: -half, width: size, height: size, style: 'mask-type:alpha' },
    tag('use', { href: '#' + iid }));
  // colour bleed: warm bounce from the floor and a warm wash from the side that faces the glow
  const bleedColor = dark ? '#FF9A68' : '#FFB48A';
  const bleedGrad = radial(bid, [[0, bleedColor, dark ? 0.55 : 0.5], [1, bleedColor, 0]],
    { cx: 0.5 + sign * 0.2, cy: 0.9, r: 0.62 });
  const hazeColor = dark ? '#7A4A3A' : '#E9C7B5';

  const k = size / 64;
  const pill = (cx, cy, s = 1) => g({ transform: 'rotate(9 ' + cx + ' ' + cy + ')' },
    rect({ class: 'bl' + i, x: f2(cx - 2.8 * s), y: f2(cy - 5.3 * s), width: f2(5.6 * s), height: f2(10.6 * s), rx: f2(2.8 * s), fill: EYE_COLOR }));
  const arc = (cx, cy) => path({
    d: 'M' + f2(cx - 3.4) + ' ' + f2(cy + 2.2) + 'Q' + cx + ' ' + f2(cy - 3.8) + ' ' + f2(cx + 3.4) + ' ' + f2(cy + 2.2),
    fill: 'none', stroke: EYE_COLOR, 'stroke-width': 2.6, 'stroke-linecap': 'round', transform: 'rotate(4 ' + cx + ' ' + cy + ')',
  });
  const eyeFor = (cx, which) => kind === 'happy' ? arc(cx, e.y)
    : kind === 'wink' ? (which === 'r' ? arc(cx, e.y) : pill(cx, e.y))
    : kind === 'wide' ? pill(cx, e.y, 1.2) : pill(cx, e.y);
  const cheek = (cx, side) => ellipse({ cx: f2(cx + side * 4.2), cy: f2(e.y + 7.2), rx: 3.6, ry: 2.2, fill: 'url(#blush)' });
  const eyeGroup = g({ transform: 'translate(' + look[0] + ' ' + look[1] + ')' },
    g({ class: 'gz' + i }, eyeFor(e.l, 'l') + eyeFor(e.r, 'r')));
  const face = g({ transform: 'translate(' + -half + ' ' + -half + ') scale(' + f2(k) + ')' },
    (blush ? cheek(e.l, -1) + cheek(e.r, 1) : '') + eyeGroup);

  const body = g({ transform: 'translate(' + f2(x) + ' ' + f2(y) + ')' },
    g({ class: 'fl' + i },
      g({ class: 'br' + i },
        g({ transform: 'rotate(' + rotate + ')' }, [
          tag('use', { href: '#' + iid, filter: 'url(#' + fid + ')' }),
          g({ mask: 'url(#' + mid + ')' }, [
            depth ? rect({ x: -half, y: -half, width: size, height: size, fill: hazeColor, opacity: f2(0.06 + depth * 0.12) }) : '',
            rect({ x: -half, y: -half, width: size, height: size, fill: 'url(#' + bid + ')' }),
          ]),
          face,
        ]))));

  const shadow = g({ class: 'sh' + i }, ellipse({
    cx: f2(x + size * 0.03), cy: f2(base + size * 0.012), rx: f2(size * (0.4 - depth * 0.04)), ry: f2(size * 0.075), fill: 'url(#shadow)',
  }));

  // ---- animation rules (transform / opacity only)
  const [fd, fa, fl] = anim.float, [bd, bl] = anim.breathe, [kd, kl] = anim.blink || [0, 0], [gd, gl, gx, gy] = anim.glance || [0, 0, 0, 0];
  const css = [
    '@keyframes bob' + i + '{0%,100%{transform:translateY(0)}50%{transform:translateY(' + -fa + 'px)}}',
    '.fl' + i + '{animation:bob' + i + ' ' + fd + 's ease-in-out ' + fl + 's infinite}',
    '@keyframes sh' + i + '{0%,100%{transform:scale(1);opacity:1}50%{transform:scale(' + f2(1 - fa * 0.016) + ');opacity:' + f2(1 - fa * 0.04) + '}}',
    '.sh' + i + '{transform-box:fill-box;transform-origin:50% 50%;animation:sh' + i + ' ' + fd + 's ease-in-out ' + fl + 's infinite}',
    '.br' + i + '{transform-box:fill-box;transform-origin:50% 100%;animation:br ' + bd + 's ease-in-out ' + bl + 's infinite}',
    kd ? '.bl' + i + '{transform-box:fill-box;transform-origin:50% 50%;animation:blink ' + kd + 's ease-in-out ' + kl + 's infinite}' : '',
    gd ? '@keyframes gz' + i + '{0%,30%{transform:translate(0,0)}36%,50%{transform:translate(' + gx + 'px,' + gy + 'px)}58%,72%{transform:translate(' + f2(-gx * 0.8) + 'px,0)}80%,100%{transform:translate(0,0)}}'
      + '.gz' + i + '{animation:gz' + i + ' ' + gd + 's ease-in-out ' + gl + 's infinite}' : '',
  ].join('');

  return { defs: filter + image + mask + bleedGrad, css, shadow, body };
}

// Shared animation keyframes and the reduced-motion switch.
export const baseCss = '@keyframes br{0%,100%{transform:scale(1,1)}50%{transform:scale(1.012,1.022)}}'
  + '@keyframes blink{0%,92%,100%{transform:scaleY(1)}95.5%{transform:scaleY(0.1)}}'
  + '@keyframes tw{0%,100%{opacity:var(--o)}50%{opacity:calc(var(--o)*0.25)}}'
  + '@keyframes dr1{0%,100%{transform:translate(0,0)}50%{transform:translate(3px,-9px)}}'
  + '@keyframes dr2{0%,100%{transform:translate(0,0)}50%{transform:translate(-4px,-6px)}}'
  + '@keyframes dr3{0%,100%{transform:translate(0,0)}50%{transform:translate(2px,-12px)}}';

export const reducedMotionCss = '@media (prefers-reduced-motion:reduce){*{animation:none!important}}';
