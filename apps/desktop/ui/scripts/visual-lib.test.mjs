import assert from 'node:assert/strict';
import test from 'node:test';
import { compareThumb, contrast, dHash, encodeThumb, GRID, over, requiredRatio, worstContrast } from './visual-lib.mjs';

const flat = (r, g, b) => Array.from({ length: GRID * GRID }, () => [r, g, b]).flat();
const baseline = (rgb) => ({ hash: dHash(rgb), thumb: encodeThumb(rgb) });

test('an identical or slightly shifted screenshot passes, a missing region fails', () => {
  const base = flat(20, 20, 30);
  const entry = baseline(base);
  assert.equal(compareThumb(base, entry).ok, true);
  assert.equal(compareThumb(base.map((v) => v + 3), entry).ok, true);
  const gone = [...base];
  for (let c = 0; c < 8; c++) for (let k = 0; k < 3; k++) gone[c * 3 + k] = 230; // 8 cells turn bright
  const result = compareThumb(gone, entry);
  assert.equal(result.ok, false);
  assert.match(result.reasons.join(' '), /regions changed/);
});

test('WCAG contrast: black on white is 21:1, grey on grey is below AA', () => {
  assert.ok(Math.abs(contrast([0, 0, 0], [255, 255, 255]) - 21) < 1e-9);
  assert.ok(contrast([150, 150, 150], [180, 180, 180]) < 4.5);
  assert.equal(requiredRatio(14, 400), 4.5);
  assert.equal(requiredRatio(24, 400), 3);
});

test('worstContrast sees a light patch behind translucent text and ignores a lone speck', () => {
  const dark = Array.from({ length: 100 }, () => [10, 10, 20]);
  assert.ok(worstContrast([240, 240, 240], 1, dark) > 10);
  const patchy = [...dark.slice(0, 80), ...Array.from({ length: 20 }, () => [235, 235, 235])];
  assert.ok(worstContrast([240, 240, 240], 1, patchy) < 4.5);
  const speck = [...dark.slice(0, 99), [235, 235, 235]];
  assert.ok(worstContrast([240, 240, 240], 1, speck) > 10);
  assert.deepEqual(over([100, 100, 100], 0.5, [0, 0, 0]), [50, 50, 50]);
});
