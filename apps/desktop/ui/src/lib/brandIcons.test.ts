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
  for (const name of ['Stripe', 'Atlassian', 'Jira', 'Trello', 'Asana', 'HubSpot', 'Shopify', 'Dropbox', 'Zoom', 'Telegram', 'WhatsApp', 'Spotify', 'Airtable', 'Zendesk', 'Docker', 'Vercel', 'Supabase', 'Google Sheets', 'notion-remote', 'TikTok']) {
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
  for (const name of ['cli:kiro', 'kiro-cli', 'Kiro CLI']) assert.deepEqual(brandIcon(name), brandIcon('kiro'), name);
  for (const name of ['cli:qoder', 'qodercli', 'Qoder CLI']) assert.deepEqual(brandIcon(name), brandIcon('qoder'), name);
  assert.equal(brandIcon('kiro').known, true);
  assert.equal(brandIcon('qoder').known, true);
  assert.equal(brandIcon('cli:copilot').known, true);
  assert.deepEqual(brandIcon('cli:copilot'), brandIcon('githubcopilot'));
  assert.equal(brandIcon('cli:kimi').known, true);
  assert.deepEqual(brandIcon('cli:kimi'), brandIcon('kimi'));
  for (const name of ['cli:vibe', 'vibe-acp', 'Mistral Vibe']) assert.deepEqual(brandIcon(name), brandIcon('mistralai'), name);
  for (const name of ['cli:goose', 'Goose']) assert.deepEqual(brandIcon(name), brandIcon('goose'), name);
  assert.equal(brandIcon('goose').path, 'M3.5 6.5 5 5l7 7-7 7-1.5-1.5L9 12ZM12 17h8.5v2H12Z');
  assert.notEqual(brandIcon('goose').path, brandIcon('kiro').path);
});

test('Kiro and Qoder use their own marks, not the generic terminal glyph', () => {
  const terminal = 'M3.5 6.5 5 5l7 7-7 7-1.5-1.5L9 12ZM12 17h8.5v2H12Z';
  for (const key of ['kiro', 'qoder']) {
    const svg = readFileSync(new URL(`../assets/brands/${key}.svg`, import.meta.url), 'utf8');
    const path = /\bd="([^"]+)"/.exec(svg)?.[1];
    assert.ok(path, key);
    assert.equal(brandIcon(key).path, path, key);
    assert.notEqual(brandIcon(key).path, terminal, key);
    assert.equal(brandIcon(key).monochrome, true, key);
  }
  assert.notEqual(brandIcon('kiro').path, brandIcon('qoder').path);
  assert.equal(brandIcon('goose').path, terminal, 'Goose has no vendor mark yet and keeps the neutral glyph');
});

test('brands whose terms do not allow their logo use an explicit neutral initial and stay offline', () => {
  for (const [id, initial] of Object.entries({ groq: 'G', together: 'T', playwright: 'P' })) {
    const b = brandIcon(id);
    assert.equal(b.known, true, `${id}: must not trigger AppLogo remote lookup`);
    assert.equal(b.path, '', id);
    assert.equal(b.monogram, initial, id);
    assert.equal(b.monochrome, true, id);
    assert.deepEqual(brandIcon(b.name), b, `${id}: display name must resolve to the same bundled initial`);
  }
});

// Apps the shipped Composio list pins that have no bundled mark. Simple Icons (CC0) dropped these
// four, and no licence for the vendors' own artwork is verified (docs/design/brand-icons.md), so
// they show the daemon-fetched Composio logo, else their initials: never a look-alike. A new gap
// in the list must be added to the audit, so this test names it.
test('the pinned Composio apps without a bundled mark are exactly the audited ones', () => {
  const curated = JSON.parse(
    readFileSync(new URL('../../../../../crates/composio/src/curated.json', import.meta.url), 'utf8')
  ) as { popular: { slug: string; name: string }[] };
  const missing = curated.popular.filter((p) => !brandIcon(p.name).known || !brandIcon(p.slug).known).map((p) => p.slug);
  assert.deepEqual(missing.sort(), ['linkedin', 'microsoft_teams', 'one_drive', 'outlook', 'salesforce']);
});

test('llama.cpp, Tavily, Firecrawl, Context7 and Exa use the vendor marks, matching the provenance files', () => {
  const colour: Record<string, boolean> = { llamacpp: true, tavily: true, context7: true, firecrawl: false, exa: false };
  for (const [key, monochrome] of Object.entries(colour)) {
    const svg = readFileSync(new URL(`../assets/brands/${key}.svg`, import.meta.url), 'utf8');
    const path = /\bd="([^"]+)"/.exec(svg)?.[1];
    const b = brandIcon(key);
    assert.ok(path, key);
    assert.equal(b.path, path, key);
    assert.equal(b.known, true, key);
    assert.equal(b.monogram, '', key);
    assert.equal(!!b.monochrome, monochrome, `${key}: monochrome marks follow the theme text colour`);
    assert.deepEqual(brandIcon(b.name), b, `${key}: display name resolves to the same mark`);
  }
  assert.deepEqual(brandIcon('Llama.cpp'), brandIcon('llamacpp'));
  assert.equal(brandIcon('firecrawl').color, '#FA5D19');
});

// The audit's licence register: every non-Simple-Icons artwork and every neutral fallback needs a row with
// a source URL, terms and a check date. Add the row (docs/design/brand-icons.md) when this fails.
test('every shipped brand artwork and neutral fallback has a recorded source, licence and date', () => {
  const doc = readFileSync(new URL('../../../../../docs/design/brand-icons.md', import.meta.url), 'utf8');
  const section = /## Licence register([\s\S]*?)\n## /.exec(doc)?.[1];
  assert.ok(section, 'docs/design/brand-icons.md must have a "Licence register" section');
  const rows = new Map<string, string[]>();
  for (const line of section.split('\n').filter(l => l.startsWith('|'))) {
    const cells = line.split('|').slice(1, -1).map(c => c.trim());
    const key = /^`([^`]+)`$/.exec(cells[1] ?? '')?.[1];
    if (key) rows.set(key, cells);
  }
  assert.ok(rows.size >= 10, 'register rows must be read, not silently skipped');
  for (const [key, cells] of rows) {
    assert.match(cells[2], /^(logo|initial|glyph)$/, `${key}: result`);
    assert.match(cells[4], /https:\/\//, `${key}: source URL`);
    assert.ok(cells[5].length >= 7, `${key}: licence or terms`);
    assert.match(cells[6], /^\d{4}-\d{2}-\d{2}$/, `${key}: check date`);
    if (key !== 'slack') assert.equal(cells[2] === 'logo' || cells[2] === 'glyph', brandIcon(key).path !== '', `${key}: result must match the registry`);
  }
  // Every hand-listed artwork in the registry and every initial.
  const source = readFileSync(new URL('./brandIcons.ts', import.meta.url), 'utf8');
  const keys = new Set([...source.matchAll(/if \(key === '([a-z0-9]+)'\)/g)].map(m => m[1]));
  const initialsBlock = /const initials[^{]*\{([^}]*)\}/.exec(source)?.[1] ?? '';
  for (const m of initialsBlock.matchAll(/^\s*([a-z0-9]+):/gm)) keys.add(m[1]);
  keys.delete('openaicodex'); // alias of the openai row
  assert.ok(keys.size >= 10, 'registry keys must be read');
  for (const key of keys) assert.ok(rows.has(key), `${key}: add a row to the licence register`);
  // Every bundled provenance file is named by its row.
  for (const file of readdirSync(new URL('../assets/brands/', import.meta.url))) {
    const row = [...rows.values()].find(cells => cells[3] === `\`${file}\``);
    assert.ok(row, `${file}: add a row naming it to the licence register`);
  }
  const si = readFileSync(new URL('../../node_modules/simple-icons/LICENSE.md', import.meta.url), 'utf8');
  assert.match(si, /CC0 1\.0/, 'Simple Icons paths are bundled on their CC0 licence');
});
