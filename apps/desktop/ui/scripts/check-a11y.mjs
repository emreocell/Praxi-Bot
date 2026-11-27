// Automated accessibility check (axe-core) over the mock-mode screens, in a headless Chrome.
//   node scripts/check-a11y.mjs [--only=<substring>] [--budget=<seconds>]
// Builds the mock bundle (VITE_MOCK=1), serves it with `vite preview`, opens every case in one
// Chrome through playwright-core, runs axe-core and compares the result with `a11y-allowlist.mjs`.
// New violations fail; allow-list entries that no longer match anything fail too (ratchet).
// Chrome is found through CHROME_BIN.  Everything temporary lives in one temp dir removed in `finally`.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { allowlist } from './a11y-allowlist.mjs';

const args = process.argv.slice(2);
const flag = (name) => args.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const only = flag('only');
const budgetMs = Number(flag('budget') ?? 120) * 1000;
const root = fileURLToPath(new URL('../', import.meta.url));
const CHROMES = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium'].filter(Boolean);
const chromePath = CHROMES.find((p) => existsSync(p));
if (!chromePath) { process.stdout.write('check:a11y needs Chrome (set CHROME_BIN)\n'); process.exit(1); }

// Screens (mock review states, see README "Mock mode").  Settings sections are separate screens.
const SETTINGS = ['general', 'appearance', 'models', 'connections', 'safety', 'system'];
const screens = [
  { name: 'home', query: 'screen=home' },
  { name: 'home-no-bots', query: 'screen=nobots', expect: '[data-testid=home-first-steps]' },
  { name: 'host-empty', query: 'screen=home&host=empty', expect: '[data-testid=host-setup-steps]' },
  { name: 'host-offline', query: 'screen=home&host=offline', expect: '[data-testid=host-empty-state]' },
  { name: 'my-bots', query: 'screen=myBots', expect: '[data-testid=my-bots]' },
  { name: 'chat', query: 'screen=bot' },
  { name: 'chat-right-panel', query: 'screen=bot&panel=1' },
  { name: 'chat-answer-actions', query: 'screen=answer-run' },
  { name: 'chat-right-panel-lite-hint', query: 'litebot=1&panel=1', act: [{ click: '[data-testid=bot-panel-lite-hint]' }] },
  { name: 'chat-mission', expect: '[data-testid=mission-chat-header]', query: 'screen=mission&panel=1' },
  { name: 'chat-mission-answers', expect: '[data-testid=mission-needs-you]', query: 'screen=mission', act: [{ click: '[data-testid=mission-header-answer]' }] },
  { name: 'chat-mission-done', expect: '[data-testid=mission-digest]', query: 'screen=mission-done', act: [{ click: '[data-testid=mission-header-report]' }] },
  { name: 'chat-approval', query: 'screen=approval' },
  { name: 'schedules', query: 'screen=schedules' },
  { name: 'needs', query: 'screen=needs' },
  { name: 'apps', query: 'screen=apps' },
  { name: 'apps-detail', query: 'screen=apps&composio=connected', expect: '[data-testid=composio-app-gmail]', act: [{ click: '[data-testid=composio-tile-gmail] button' }] },
  { name: 'apps-rest-apis', query: 'screen=apps', expect: '[data-testid=openapi-apps]', act: [{ click: '[data-testid=apps-rest]' }] },
  { name: 'new-bot-dialog', query: 'screen=newbot' },
  { name: 'bot-edit-dialog', query: 'screen=botform' },
  { name: 'bot-profile-dialog', query: 'screen=botprofile' },
  { name: 'skills-dialog', query: 'screen=skills' },
  { name: 'first-run-wizard', query: 'screen=onboarding' },
  { name: 'first-run-freedom', query: 'screen=onboarding&frstep=freedom' },
  { name: 'first-run-bot', query: 'screen=onboarding&frstep=bot' },
  // Views opened by a few clicks/keys (act: click a selector, type into one, or press a key).
  { name: 'settings-search', expect: '[data-autofocus]', query: 'screen=settings&section=general', act: [{ type: ['[data-autofocus]', 'theme'] }] },
  { name: 'new-bot-identity', expect: '[data-testid=new-bot-name]', query: 'screen=newbot', act: [{ click: '[data-testid=new-bot-start-blank]' }] },
  { name: 'new-bot-access', expect: '[data-testid=new-bot-summary]', query: 'screen=newbot', act: [{ click: '[data-testid=new-bot-start-blank]' }, { type: ['[data-testid=new-bot-name]', 'Nova'] }, { click: '[data-testid=new-bot-next]' }] },
  { name: 'settings-search-empty', query: 'screen=settings&section=general', act: [{ type: ['[data-autofocus]', 'zzzqqq'] }] },
  { name: 'vault-grants', expect: '[data-testid=vault-grants-panel]', scope: '[data-testid=vault-grants-panel]', query: 'screen=vault', act: [{ click: '[data-testid=vault-grants-open]' }] },
  { name: 'vault-grants-revoke', expect: '[data-testid=confirm-dialog]', scope: '[data-testid=confirm-dialog]', query: 'screen=vault', act: [{ click: '[data-testid=vault-grants-open]' }, { click: '[data-testid^=vault-grant-revoke-]' }] },
  { name: 'help-dialog', expect: '[data-testid=help-center]', query: 'screen=home', act: [{ press: 'Shift+?' }] },
  { name: 'run-location-popover', expect: '[data-testid=home-status]', query: 'screen=home&menu=1' },
  { name: 'add-server-wizard', expect: '[data-testid=add-server-live]', query: 'screen=home&menu=1', act: [{ click: '[data-testid=computer-add]' }] },
  { name: 'add-server-choose', query: 'screen=home&menu=1', act: [{ click: '[data-testid=computer-add]' }, { click: '[data-testid=add-server-next]', optional: true }] },
  { name: 'schedule-builder', expect: 'form', query: 'screen=schedules', act: [{ click: '[data-testid=schedules-empty-new], [data-testid=schedules-new]', optional: true }, { clickText: /new schedule/i, optional: true }] },
  { name: 'run-timeline', expect: '[role=dialog]', query: 'screen=changes', act: [{ click: '[data-testid=run-timeline]' }] },
  { name: 'live-viewer', expect: '[data-testid=live-viewer]', query: 'screen=chat&panel=1', act: [{ click: '[data-testid=bot-computer-show]' }, { click: '[data-testid=bot-computer-big]' }] },
  { name: 'live-viewer-terminals', expect: '[data-testid=live-viewer-terminals]', query: 'screen=chat&panel=1&lvTerm=1', act: [{ click: '[data-testid=bot-computer-show]' }, { click: '[data-testid=bot-computer-big]' }] },
  { name: 'live-viewer-menu', expect: '[data-testid=live-viewer-tools]', query: 'screen=chat&panel=1', act: [{ click: '[data-testid=bot-computer-show]' }, { click: '[data-testid=bot-computer-big]' }, { click: '[data-testid=live-viewer-view]' }] },
  // Right-click menus (act: rightClick a selector); the keyboard pass skips them because Tab closes a menu.
  { name: 'context-menu-message', expect: '[role=menu][data-testid=context-menu]', skipRing: true, scope: '[data-testid=context-menu]', query: 'screen=chat', act: [{ rightClick: '[data-message-row=assistant]' }] },
  { name: 'context-menu-user-message', expect: '[role=menu][data-testid=context-menu]', skipRing: true, scope: '[data-testid=context-menu]', query: 'screen=chat', act: [{ rightClick: '[data-message-row=user]' }] },
  { name: 'context-menu-chat-row', expect: '[role=menu][data-testid=context-menu]', skipRing: true, scope: '[data-testid=context-menu]', query: 'screen=bot&panel=1', act: [{ click: '#bot-panel-tab-library' }, { rightClick: '[data-testid=chat-row-panel] button' }] },
  { name: 'context-menu-bot-row', expect: '[role=menu][data-testid=context-menu]', skipRing: true, scope: '[data-testid=context-menu]', query: 'screen=bot', act: [{ rightClick: '[data-bot-id] button' }] },
  { name: 'command-popover', expect: '[role=listbox]', query: 'screen=chat', act: [{ type: ['textarea', '/'] }] },
  { name: 'mention-popover', expect: '[role=listbox]', query: 'screen=chat', act: [{ type: ['textarea', '@'] }] },
  ...SETTINGS.map((s) => ({ name: `settings-${s}`, query: `screen=settings&section=${s}` }))
];
// Two themes (one light, one dark) in English, plus two other locales (text differs, structure does not).
const themes = ['clay', 'obsidian'];
const cases = [];
for (const s of screens) for (const theme of themes) cases.push({ name: `${s.name}/${theme}/en`, query: `${s.query}&theme=${theme}&lang=en`, act: s.act, expect: s.expect, scope: s.scope });
cases.push({ name: 'home/clay/tr', query: 'screen=home&theme=clay&lang=tr' }, { name: 'settings-general/clay/de', query: 'screen=settings&section=general&theme=clay&lang=de' });
const selected = only ? cases.filter((c) => c.name.includes(only)) : cases;

const killTree = (pid) => {
  if (!pid) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  else { try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ } }
};
const freePort = () => new Promise((ok, fail) => {
  const s = createServer(); s.unref(); s.on('error', fail);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
});

const axeSource = readFileSync(resolve(root, 'node_modules/axe-core/axe.min.js'), 'utf8');
const tmp = mkdtempSync(join(tmpdir(), 'praxi-a11y-'));
const started = Date.now();
let preview; let browser;
const found = new Map(); // key -> { rule, impact, help, selector, html, screens:Set }
let failed = false;
try {
  const vite = resolve(root, 'node_modules/vite/bin/vite.js');
  const dist = join(tmp, 'dist');
  const build = spawnSync(process.execPath, [vite, 'build', '--outDir', dist, '--emptyOutDir'],
    { cwd: root, env: { ...process.env, VITE_MOCK: '1' }, encoding: 'utf8', windowsHide: true, timeout: 120000 });
  if (build.status !== 0) throw new Error(`vite build failed:\n${build.stdout}\n${build.stderr}`);
  const port = await freePort();
  preview = spawn(process.execPath, [vite, 'preview', '--outDir', dist, '--port', String(port), '--strictPort', '--host', '127.0.0.1'],
    { cwd: root, windowsHide: true, stdio: 'ignore' });
  for (let n = 0; n < 50; n++) {
    try { if ((await globalThis.fetch(`http://127.0.0.1:${port}/`)).ok) break; } catch { /* not up yet */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath: chromePath, headless: true, args: ['--no-sandbox'] });
  const context = await browser.newContext({ viewport: { width: 1400, height: 900 }, reducedMotion: 'reduce', deviceScaleFactor: 1 });
  let next = 0;
  let budgetHit = false;
  const runCase = async (c) => {
    const page = await context.newPage();
    try {
      await page.goto(`http://127.0.0.1:${port}/?mock=1&rt=ignore&${c.query}`, { waitUntil: 'load' });
      await page.waitForSelector('#root > *', { timeout: 15000 });
      await page.waitForTimeout(500); // mock data and entry transitions settle
      for (const a of c.act ?? []) {
        try {
          if (a.press) await page.keyboard.press(a.press);
          else if (a.type) await page.locator(a.type[0]).first().fill(a.type[1], { timeout: 5000 });
          else if (a.clickText) await page.getByRole('button', { name: a.clickText }).first().click({ timeout: 5000 });
          else if (a.rightClick) await page.locator(a.rightClick).first().click({ button: 'right', timeout: 5000 });
          else await page.locator(a.click).first().click({ timeout: 5000 });
        } catch (err) { if (!a.optional) throw err; }
        await page.waitForTimeout(400);
      }
      if (c.expect) await page.waitForSelector(c.expect, { timeout: 5000 }); // the view under test really opened
      await page.evaluate(axeSource);
      const result = await page.evaluate((scope) => globalThis.axe.run(scope ? globalThis.document.querySelector(scope) : globalThis.document, {
        runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice'] },
        resultTypes: ['violations']
      }), c.scope);
      for (const v of result.violations) {
        for (const node of v.nodes) {
          const selector = node.target.join(' >> ');
          const key = `${v.id} ${selector}`;
          const e = found.get(key) ?? { rule: v.id, impact: v.impact, help: v.help, selector, screens: new Set(), html: node.html.slice(0, 160) };
          e.screens.add(c.name); found.set(key, e);
        }
      }
    } catch (err) {
      process.stdout.write(`FAIL ${c.name}: ${err.message.split('\n')[0]}\n`); failed = true;
    } finally { await page.close(); }
  };
  // A few pages at a time keeps the whole run well under a minute; results are merged per violation.
  await Promise.all(Array.from({ length: 4 }, async () => {
    while (next < selected.length) {
      if (Date.now() - started > budgetMs) { budgetHit = true; return; }
      await runCase(selected[next++]);
    }
  }));
  if (budgetHit) { process.stdout.write(`BUDGET exhausted after ${next} cases\n`); failed = true; }

  // Keyboard pass: Tab through the new views in four themes; every stop must show a focus indicator.
  const ringThemes = ['clay', 'obsidian', 'contrast-dark', 'contrast-light'];
  const ringScreens = screens.filter((s) => s.expect && !s.skipRing && (!only || s.name.includes(only)));
  const ringProblems = new Map();
  const combos = ringScreens.flatMap((sc) => ringThemes.map((theme) => ({ sc, theme })));
  let nextCombo = 0;
  const ringWorker = async () => { while (nextCombo < combos.length) {
    const { sc, theme } = combos[nextCombo++];
    const page = await context.newPage();
    try {
      await page.goto(`http://127.0.0.1:${port}/?mock=1&rt=ignore&${sc.query}&theme=${theme}&lang=en`, { waitUntil: 'load' });
      await page.waitForSelector('#root > *', { timeout: 15000 });
      await page.waitForTimeout(400);
      for (const a of sc.act ?? []) {
        try {
          if (a.press) await page.keyboard.press(a.press);
          else if (a.type) await page.locator(a.type[0]).first().fill(a.type[1], { timeout: 5000 });
          else if (a.clickText) await page.getByRole('button', { name: a.clickText }).first().click({ timeout: 5000 });
          else await page.locator(a.click).first().click({ timeout: 5000 });
        } catch { /* optional step */ }
        await page.waitForTimeout(300);
      }
      const seen = new Set();
      for (let i = 0; i < 45; i++) {
        await page.keyboard.press('Tab');
        const info = await page.evaluate(() => {
          const el = globalThis.document.activeElement;
          if (!el || el === globalThis.document.body) return null;
          const cs = globalThis.getComputedStyle(el);
          const r = el.getBoundingClientRect();
          const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
          const shadow = cs.boxShadow && cs.boxShadow !== 'none';
          // The indicator may sit on a wrapper or a child (focus-within / :focus-visible .preview): compare styles with and without focus.
          const group = [el, ...Array.from(el.querySelectorAll('*')).slice(0, 12)];
          for (let p = el.parentElement, n = 0; p && n < 3; p = p.parentElement, n++) group.push(p);
          const sig = () => group.map((g) => { const c = globalThis.getComputedStyle(g); return `${c.outlineStyle}${c.outlineWidth}${c.boxShadow}${c.borderColor}${c.backgroundColor}`; }).join('|');
          const on = sig(); el.blur(); const off = sig(); el.focus({ focusVisible: true });
          const name = el.getAttribute('data-testid') || el.getAttribute('aria-label') || (el.textContent || '').trim().slice(0, 30) || el.tagName;
          return { key: `${el.tagName.toLowerCase()}[${name}]`, ring: outline || shadow || on !== off, w: r.width, h: r.height, inline: cs.display === 'inline' };
        });
        if (!info) continue;
        if (seen.has(info.key)) break; // looped back
        seen.add(info.key);
        if (!info.ring) { const k = `ring ${sc.name} ${info.key}`; ringProblems.set(k, [...(ringProblems.get(k) ?? []), theme]); }
      }
    } catch (err) { process.stdout.write(`FAIL ring ${sc.name}/${theme}: ${err.message.split('\n')[0]}\n`); failed = true; }
    finally { await page.close(); }
  } };
  await Promise.all(Array.from({ length: 4 }, ringWorker));
  for (const [k, t] of ringProblems) { process.stdout.write(`NO FOCUS RING: ${k} (${t.join(', ')})\n`); failed = true; }

  // Focus return: Escape on a dialog/popover puts focus back on the control that opened it.
  const returnChecks = [
    { name: 'run location popover', query: 'screen=home', open: async (pg) => { await pg.locator('[data-testid=computer-picker]').focus(); await pg.keyboard.press('Enter'); }, within: '[data-testid=home-status]', back: '[data-testid=computer-picker]' },
    { name: 'help dialog', query: 'screen=home', open: async (pg) => { await pg.locator('[data-testid=computer-picker]').focus(); await pg.keyboard.press('Shift+?'); }, within: '[data-testid=help-center]', back: '[data-testid=computer-picker]' },
    { name: 'add server wizard', query: 'screen=home&menu=1', open: async (pg) => { await pg.locator('[data-testid=computer-add]').first().click(); }, within: '[data-testid=add-server]', back: '[data-testid=computer-picker]' }
  ];
  for (const rc of returnChecks) {
    if (only && !rc.name.includes(only)) continue;
    const page = await context.newPage();
    try {
      await page.goto(`http://127.0.0.1:${port}/?mock=1&rt=ignore&${rc.query}&theme=clay&lang=en`, { waitUntil: 'load' });
      await page.waitForSelector('#root > *', { timeout: 15000 });
      await page.waitForTimeout(500);
      await rc.open(page);
      await page.waitForSelector(rc.within, { timeout: 5000 });
      await page.waitForTimeout(300);
      const inside = await page.evaluate((sel) => !!globalThis.document.querySelector(sel)?.contains(globalThis.document.activeElement) || !!globalThis.document.activeElement?.closest('[role=dialog]'), rc.within);
      if (!inside) { process.stdout.write(`FOCUS NOT MOVED INTO ${rc.name}\n`); failed = true; }
      await page.keyboard.press('Escape');
      await page.waitForTimeout(500);
      const gone = await page.locator(rc.within).count();
      const ok = await page.evaluate((sel) => { const a = globalThis.document.activeElement; return !!a && a !== globalThis.document.body && (!sel || a.matches(sel)); }, rc.back);
      if (gone) { process.stdout.write(`ESC DID NOT CLOSE ${rc.name}\n`); failed = true; }
      else if (!ok) { process.stdout.write(`FOCUS NOT RETURNED after ${rc.name}\n`); failed = true; }
    } catch (err) { process.stdout.write(`FAIL focus-return ${rc.name}: ${err.message.split('\n')[0]}\n`); failed = true; }
    finally { await page.close(); }
  }

  const matches = (entry, v) => entry.rule === v.rule && v.selector.includes(entry.selector)
    && (!entry.screens || [...v.screens].some((s) => entry.screens.some((n) => s.startsWith(`${n}/`))));
  const used = new Set();
  const fresh = [];
  for (const v of found.values()) {
    const hit = allowlist.filter((a) => matches(a, v));
    if (hit.length) hit.forEach((a) => used.add(a)); else fresh.push(v);
  }
  const stale = only ? [] : allowlist.filter((a) => !used.has(a));
  for (const a of allowlist) if (!a.reason) { process.stdout.write(`allow-list entry ${a.rule} / ${a.selector} has no reason\n`); failed = true; }
  const secs = ((Date.now() - started) / 1000).toFixed(0);
  process.stdout.write(`a11y: ${selected.length} cases, ${found.size} violating nodes (${found.size - fresh.length} allow-listed), ${secs}s\n`);
  const byRule = new Map();
  for (const v of fresh) byRule.set(v.rule, [...(byRule.get(v.rule) ?? []), v]);
  for (const [rule, list] of byRule) {
    process.stdout.write(`\nNEW ${rule} (${list[0].impact}): ${list[0].help}\n`);
    for (const v of list) process.stdout.write(`  ${v.selector}\n    ${v.html}\n    on: ${[...v.screens].slice(0, 6).join(', ')}${v.screens.size > 6 ? ` +${v.screens.size - 6}` : ''}\n`);
  }
  for (const a of stale) process.stdout.write(`\nSTALE allow-list entry (fixed? delete it): ${a.rule} / ${a.selector}\n`);
  if (fresh.length || stale.length) failed = true;
  if (!failed) process.stdout.write('a11y OK\n');
} finally {
  try { await browser?.close(); } catch { /* ignore */ }
  killTree(preview?.pid);
  rmSync(tmp, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
