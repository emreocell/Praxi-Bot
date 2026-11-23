// Pure helpers for the visual regression check (scripts/check-visual.mjs): no browser, no I/O.
// A screenshot is reduced to a GRID x GRID RGB thumbnail (box average). The baseline stores that
// thumbnail (base64, ~1 KB) and a perceptual hash derived from it, instead of a PNG.

import { Buffer } from 'node:buffer';

export const GRID = 16;

/** Thresholds are deliberately tolerant: fonts and anti-aliasing differ a little between operating systems. */
export const THRESHOLDS = {
  /** Differing bits of the 240-bit gradient hash. */
  hamming: 28,
  /** Mean absolute colour difference over all cells, 0..255. */
  meanDiff: 8,
  /** A cell counts as "changed" above this mean channel difference ... */
  cellDiff: 32,
  /** ... and the screen fails when more than this many cells changed (a missing popover or card). */
  changedCells: 4
};

export const luma = (r, g, b) => 0.2126 * r + 0.7152 * g + 0.0722 * b;

/** Gradient hash: one bit per horizontal neighbour pair of the luminance grid (GRID rows x (GRID-1) pairs). */
export function dHash(rgb) {
  const lum = [];
  for (let i = 0; i < GRID * GRID; i++) lum.push(luma(rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]));
  let bits = '';
  for (let y = 0; y < GRID; y++) for (let x = 0; x < GRID - 1; x++) bits += lum[y * GRID + x + 1] > lum[y * GRID + x] ? '1' : '0';
  let hex = '';
  for (let i = 0; i < bits.length; i += 4) hex += parseInt(bits.slice(i, i + 4), 2).toString(16);
  return hex;
}

export function hamming(a, b) {
  if (a.length !== b.length) return Infinity;
  let n = 0;
  for (let i = 0; i < a.length; i++) {
    let v = parseInt(a[i], 16) ^ parseInt(b[i], 16);
    while (v) { n += v & 1; v >>= 1; }
  }
  return n;
}

export const encodeThumb = (rgb) => Buffer.from(Uint8Array.from(rgb)).toString('base64');
export const decodeThumb = (b64) => [...Buffer.from(b64, 'base64')];

/** Compare a fresh thumbnail with a baseline entry `{ hash, thumb }`. */
export function compareThumb(rgb, baseline, limits = THRESHOLDS) {
  const old = decodeThumb(baseline.thumb);
  if (old.length !== rgb.length) return { ok: false, reason: 'baseline has a different size' };
  const dist = hamming(dHash(rgb), baseline.hash);
  let total = 0;
  let changedCells = 0;
  for (let c = 0; c < GRID * GRID; c++) {
    let cell = 0;
    for (let k = 0; k < 3; k++) cell += Math.abs(rgb[c * 3 + k] - old[c * 3 + k]);
    cell /= 3;
    total += cell;
    if (cell > limits.cellDiff) changedCells++;
  }
  const meanDiff = total / (GRID * GRID);
  const reasons = [];
  if (dist > limits.hamming) reasons.push(`layout hash differs in ${dist} bits (max ${limits.hamming})`);
  if (meanDiff > limits.meanDiff) reasons.push(`average colour changed by ${meanDiff.toFixed(1)} (max ${limits.meanDiff})`);
  if (changedCells > limits.changedCells) reasons.push(`${changedCells} regions changed a lot (max ${limits.changedCells})`);
  return { ok: reasons.length === 0, reasons, hamming: dist, meanDiff, changedCells };
}

// ---- WCAG 2.x contrast -------------------------------------------------------------------------

const lin = (v) => { const s = v / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; };
export const relLuminance = (r, g, b) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
export function contrast(a, b) {
  const la = relLuminance(...a);
  const lb = relLuminance(...b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** Alpha-composite `fg` ([r,g,b], opacity 0..1) over an opaque background. */
export const over = (fg, alpha, bg) => fg.map((v, i) => v * alpha + bg[i] * (1 - alpha));

/** WCAG AA: 3:1 for large text (24 px, or 18.66 px bold), 4.5:1 otherwise. */
export const requiredRatio = (fontPx, weight) => (fontPx >= 24 || (fontPx >= 18.66 && weight >= 700) ? 3 : 4.5);

/**
 * Worst contrast of one text run against the pixels behind it. `pixels` are [r,g,b] samples of the
 * screenshot taken with the text hidden. The 5th-percentile sample is used so a single stray
 * pixel (an icon edge, a dither speck) does not decide, while a genuinely light patch still does.
 */
export function worstContrast(fg, alpha, pixels) {
  if (!pixels.length) return Infinity;
  const ratios = pixels.map((p) => contrast(over(fg, alpha, p), p)).sort((x, y) => x - y);
  return ratios[Math.floor(ratios.length * 0.05)];
}
