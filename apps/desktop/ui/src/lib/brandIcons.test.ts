import assert from 'node:assert/strict';
import { test } from 'node:test';
import { siGithub, siGmail, siLinear, siGooglegemini } from 'simple-icons';
import { brandIcon } from './brandIcons.ts';

test('known brands use real simple-icons paths and official brand colors', () => {
  for (const [name, icon] of [['GitHub', siGithub], ['Gmail', siGmail], ['linear', siLinear], ['gemini', siGooglegemini]] as const) {
    const result = brandIcon(name);
    assert.equal(result.known, true);
    assert.equal(result.color, `#${icon.hex}`);
    assert.equal(result.path, icon.path);
    assert.equal(result.monogram, '');
  }
  assert.equal(brandIcon('Google Drive').known, true);
  assert.equal(brandIcon('claude-code-cli').name, 'Claude');
  assert.equal(brandIcon('Slack').color, '#4A154B');
  assert.ok(brandIcon('OpenAI').path.length > 1000);
});

test('unknown servers receive a deterministic colored monogram with safe empty fallback', () => {
  const first = brandIcon('My private server');
  assert.equal(first.known, false);
  assert.equal(first.monogram, 'MY');
  assert.match(first.color, /^#[0-9A-F]{6}$/i);
  assert.deepEqual(brandIcon('My private server'), first);
  assert.equal(brandIcon(' ').monogram, '?');
});
