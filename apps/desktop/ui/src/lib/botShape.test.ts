import assert from 'node:assert/strict';
import { test } from 'node:test';
import { botShape } from '../../../../../packages/ui/src/botShape.ts';

test('bot silhouette is stable across calls and independent of display properties', () => {
  const original = botShape('atlas-stable-id');
  botShape('another-bot');
  assert.deepEqual(botShape('atlas-stable-id'), original);
  assert.match(original.path, /^M[\d.]+/);
  assert.ok(Number.isFinite(original.eyeY));
});

test('bot IDs produce distinct bounded silhouettes, including empty and unicode IDs', () => {
  const shapes = Array.from({ length: 64 }, (_, i) => botShape(`bot-${i}`));
  assert.ok(new Set(shapes.map(shape => shape.path)).size > 60);
  for (const id of ['', '🌱-nova', 'ı-İ-atlas', 'x'.repeat(1024)]) {
    const shape = botShape(id);
    assert.deepEqual(shape, botShape(id));
    const coordinates = shape.path.match(/\d+(?:\.\d+)?/g)?.map(Number) ?? [];
    assert.ok(coordinates.every(n => Number.isFinite(n) && n >= 0 && n <= 72));
  }
});
