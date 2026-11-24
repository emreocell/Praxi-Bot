import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync, readdirSync } from 'node:fs';
import { siGithub, siGmail, siLinear, siGooglegemini } from 'simple-icons';
import { bareBrandColor, brandIcon, hexLuminance } from './brandIcons.ts';

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

test('the popular apps of the Apps grid have real marks, not monograms', () => {
  for (const name of ['Stripe', 'Atlassian', 'Jira', 'Trello', 'Asana', 'HubSpot', 'Shopify', 'Dropbox', 'Zoom', 'Telegram', 'WhatsApp', 'Spotify', 'Airtable', 'Zendesk', 'Docker', 'Vercel', 'Supabase', 'Google Sheets', 'notion-remote']) {
    const b = brandIcon(name);
    assert.equal(b.known, true, name);
    assert.ok(b.path.length > 20, name);
    assert.equal(b.monogram, '', name);
  }
});

test('unknown servers receive a deterministic colored monogram with safe empty fallback', () => {
  const first = brandIcon('My private server');
  assert.equal(first.known, false);
  assert.equal(first.monogram, 'MY');
  assert.match(first.color, /^#[0-9A-F]{6}$/i);
  assert.deepEqual(brandIcon('My private server'), first);
  assert.equal(brandIcon(' ').monogram, '?');
});

test('bare logos that would vanish on a surface follow the text colour', () => {
  assert.equal(bareBrandColor('#181717'), 'currentColor'); // GitHub black
  assert.equal(bareBrandColor('#FFFFFF'), 'currentColor');
  assert.equal(bareBrandColor('#D97757'), '#D97757');
  assert.equal(bareBrandColor('not a colour'), 'not a colour'); // unparsable: kept as is
  assert.ok(hexLuminance('#000000') < hexLuminance('#808080'));
  assert.ok(hexLuminance('#808080') < hexLuminance('#ffffff'));
  assert.equal(brandIcon('openrouter').known, true);
  assert.equal(brandIcon('deepseek').known, true);
});

test('every registered engine and shipped provider has bundled artwork at every lookup boundary', async () => {
  const { brandOf } = await import('./cliCards.ts');
  const { providerBrand, productBrandKey } = await import('./modelBrand.ts');
  const { FALLBACK_CATALOG } = await import('./providerView.ts');
  const registry = readFileSync(new URL('../../../../../crates/cli-provider/src/registry.rs', import.meta.url), 'utf8');
  const engines = [...registry.matchAll(/CliEngineSpec\s*\{\s*id:\s*"([^"]+)"/g)].map(m => m[1]);
  assert.ok(engines.length > 0, 'engine registry must be read, not silently skipped');
  for (const id of engines) {
    for (const key of [id, `cli:${id}`, brandOf(id), providerBrand(`cli:${id}`).company, productBrandKey(`cli:${id}`, 'default')]) {
      const b = brandIcon(key);
      assert.equal(b.known, true, `${id} -> ${key}`);
      assert.ok(b.path.length > 0 && b.monogram === '', `${id} -> ${key}`);
      assert.equal(b.monochrome, true, `${id} -> ${key} must follow theme text`);
    }
  }
  const catalog = new URL('../../../../../providers/catalog/', import.meta.url);
  const providers = readdirSync(catalog).filter(f => f.endsWith('.toml')).map(f => {
    const id = /^id\s*=\s*"([^"]+)"/m.exec(readFileSync(new URL(f, catalog), 'utf8'))?.[1];
    assert.ok(id, `${f} must declare a provider id`);
    return id;
  });
  const localServers = readFileSync(new URL('./localServers.ts', import.meta.url), 'utf8');
  const localIds = /const KNOWN = new Set\(\[([^\]]+)\]/.exec(localServers)?.[1];
  assert.ok(localIds, 'local provider inventory must be read');
  const ids = new Set([...providers, ...FALLBACK_CATALOG, ...localIds.matchAll(/"([^"]+)"/g)].map(id => typeof id === 'string' ? id : id[1]));
  for (const id of [...ids, 'openai', 'claude-code', 'codex', 'gemini-cli']) {
    for (const key of [id, providerBrand(id).company, productBrandKey(id, 'default')]) {
      const b = brandIcon(key);
      assert.equal(b.known, true, `${id} -> ${key}: add a licensed mark or documented initial`);
      assert.ok(b.path || /^[A-Z]$/.test(b.monogram), `${id} -> ${key}`);
      assert.equal(b.monochrome, true, `${id} -> ${key}`);
    }
  }
  assert.equal(brandIcon(providerBrand('claude-code').company).name, 'Claude');
  assert.equal(brandIcon(providerBrand('anthropic').company).name, 'Anthropic');
});

test('Antigravity uses the official press path rather than an approximation', () => {
  const svg = readFileSync(new URL('../assets/brands/antigravity.svg', import.meta.url), 'utf8');
  const path = /\bd="([^"]+)"/.exec(svg)?.[1];
  assert.ok(path);
  assert.equal(brandIcon('agy').path, path);
  assert.deepEqual(brandIcon('cli:agy'), brandIcon('antigravity'));
  assert.equal(brandIcon('cli:qwen').known, true);
  assert.deepEqual(brandIcon('cli:qwen'), brandIcon('qwen'));
  assert.equal(brandIcon('cli:copilot').known, true);
  assert.deepEqual(brandIcon('cli:copilot'), brandIcon('githubcopilot'));
});

test('unlicensed brand artwork uses an explicit neutral initial and stays offline', () => {
  for (const [id, initial] of Object.entries({ groq: 'G', together: 'T', llamacpp: 'L', playwright: 'P', exa: 'E', tavily: 'T', firecrawl: 'F', context7: 'C' })) {
    const b = brandIcon(id);
    assert.equal(b.known, true, `${id}: must not trigger AppLogo remote lookup`);
    assert.equal(b.path, '', id);
    assert.equal(b.monogram, initial, id);
    assert.equal(b.monochrome, true, id);
    assert.deepEqual(brandIcon(b.name), b, `${id}: display name must resolve to the same bundled initial`);
  }
});
