// One-off: after screenshots for owner u18 (Settings › Connections cards).
//   node scripts/capture-u18-after.mjs
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../', import.meta.url));
const outDir = resolve(root, '../../../docs/screenshots/u18');
const CHROMES = [process.env.CHROME_BIN, '/home/webioo/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome',
  '/usr/bin/google-chrome', '/usr/bin/chromium'].filter(Boolean);
const chromePath = CHROMES.find((p) => existsSync(p));
if (!chromePath) {
  process.stdout.write('capture-u18-after: set CHROME_BIN\n');
  process.exit(1);
}

const shots = [
  { file: 'after-connect-mail.png', testId: 'connect-mail-card', advanced: false },
  { file: 'after-email-card.png', testId: 'email-card', advanced: false },
  { file: 'after-email-channel.png', testId: 'email-channel-card', advanced: true },
  { file: 'after-mail-guard.png', testId: 'mail-guard', advanced: true }
];

const freePort = () => new Promise((ok, fail) => {
  const s = createServer();
  s.unref();
  s.on('error', fail);
  s.listen(0, '127.0.0.1', () => {
    const { port } = s.address();
    s.close(() => ok(port));
  });
});

const tmp = mkdtempSync(join(tmpdir(), 'u18-shots-'));
let preview;
let browser;
try {
  const vite = resolve(root, 'node_modules/vite/bin/vite.js');
  const dist = join(tmp, 'dist');
  const build = spawnSync(process.execPath, [vite, 'build', '--outDir', dist, '--emptyOutDir'],
    { cwd: root, env: { ...process.env, VITE_MOCK: '1' }, encoding: 'utf8', windowsHide: true, timeout: 240000 });
  if (build.status !== 0) throw new Error(`vite build failed:\n${build.stdout}\n${build.stderr}`);
  const port = await freePort();
  preview = spawn(process.execPath, [vite, 'preview', '--outDir', dist, '--port', String(port), '--strictPort', '--host', '127.0.0.1'],
    { cwd: root, windowsHide: true, stdio: 'ignore' });
  for (let n = 0; n < 50; n++) {
    try {
      if ((await globalThis.fetch(`http://127.0.0.1:${port}/`)).ok) break;
    } catch { /* wait */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath: chromePath, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await page.goto(`http://127.0.0.1:${port}/?mock=1&rt=ignore&lang=en&theme=obsidian&screen=settings&section=connections`, { waitUntil: 'load' });
  await page.waitForSelector('[role="dialog"]', { timeout: 30000 });
  mkdirSync(outDir, { recursive: true });
  let advancedOpen = false;
  for (const { file, testId, selector, advanced } of shots) {
    if (advanced && !advancedOpen) {
      await page.getByTestId('settings-show-advanced').click();
      advancedOpen = true;
      await delay(300);
    }
    const el = page.locator(selector ?? `[data-testid="${testId}"]`);
    await el.scrollIntoViewIfNeeded({ timeout: 60000 });
    await delay(200);
    writeFileSync(join(outDir, file), await el.screenshot({ type: 'png' }));
    process.stdout.write(`wrote ${file}\n`);
  }
  await page.goto(`http://127.0.0.1:${port}/?mock=1&rt=ignore&lang=en&theme=obsidian&screen=settings&section=computer`, { waitUntil: 'load' });
  await page.waitForSelector('[role="dialog"]', { timeout: 30000 });
  const cal = page.locator('[data-card="calendar"]');
  await cal.scrollIntoViewIfNeeded();
  writeFileSync(join(outDir, 'after-calendar-form.png'), await cal.screenshot({ type: 'png' }));
  process.stdout.write('wrote after-calendar-form.png\n');
  const web = page.locator('[data-testid="web-search-card"]');
  await web.scrollIntoViewIfNeeded();
  writeFileSync(join(outDir, 'after-web-search.png'), await web.screenshot({ type: 'png' }));
  process.stdout.write('wrote after-web-search.png\n');
} finally {
  if (browser) await browser.close();
  if (preview) preview.kill();
  rmSync(tmp, { recursive: true, force: true });
}