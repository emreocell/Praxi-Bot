/* global document, FileReader, createImageBitmap, OffscreenCanvas, Blob, atob */
// Regenerates the screenshots of the user guide from the mock-mode app, in headless Chrome.
//   pnpm --dir apps/desktop/ui docs:screenshots [--lang=en,tr] [--only=<substring>] [--out=<dir>] [--quality=0.9]
//
// Builds the mock bundle (VITE_MOCK=1), serves it with `vite preview` and opens every screen of ./screens.mjs
// at a fixed viewport, theme, time zone and clock, once per language, then writes
// `<out>/<lang>/<name>.webp` (default `docs/guide/images`). The WebP is encoded by Chrome itself, so there is
// no extra dependency. See README.md in this folder. Chrome is found through CHROME_BIN.
import { Buffer } from 'node:buffer';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { SCREENS } from './screens.mjs';

const args = process.argv.slice(2);
const flag = (name) => args.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const root = fileURLToPath(new URL('../../', import.meta.url));
const outDir = resolve(flag('out') ?? resolve(root, '../../../docs/guide/images'));
const only = flag('only');
const quality = Number(flag('quality') ?? 0.9);

// Fixed state, so a refresh only changes an image when the UI changed.
const VIEWPORT = { width: 1280, height: 800 };
const THEME = 'obsidian';
const TIME_ZONE = 'UTC';
const NOW = new Date('2026-11-27T09:41:00Z');
const LANGUAGES = { en: 'en-US', tr: 'tr-TR' };
const languages = (flag('lang') ?? Object.keys(LANGUAGES).join(',')).split(',');
for (const l of languages) {
  if (!LANGUAGES[l]) { process.stdout.write(`unknown language "${l}" (use ${Object.keys(LANGUAGES).join(', ')})\n`); process.exit(1); }
}

const CHROMES = [process.env.CHROME_BIN, '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  'C:/Program Files/Google/Chrome/Application/chrome.exe', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome']
  .filter(Boolean);
const chromePath = CHROMES.find((p) => existsSync(p));
if (!chromePath) { process.stdout.write('docs:screenshots needs Chrome (set CHROME_BIN)\n'); process.exit(1); }

const killTree = (pid) => {
  if (!pid) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  else { try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ } }
};
const freePort = () => new Promise((ok, fail) => {
  const s = createServer(); s.unref(); s.on('error', fail);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
});

// Runs in the page: PNG (base64) in, WebP (base64) out.
const toWebp = async ({ png, quality: q }) => {
  const bytes = Uint8Array.from(atob(png), (c) => c.charCodeAt(0));
  const bmp = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
  const canvas = new OffscreenCanvas(bmp.width, bmp.height);
  canvas.getContext('2d').drawImage(bmp, 0, 0);
  const blob = await canvas.convertToBlob({ type: 'image/webp', quality: q });
  const reader = new FileReader();
  const done = new Promise((ok) => { reader.onload = () => ok(String(reader.result).split(',')[1]); });
  reader.readAsDataURL(blob);
  return done;
};

const selected = SCREENS.filter((s) => !only || s.name.includes(only));
if (selected.length === 0) { process.stdout.write(`no screen matches --only=${only}\n`); process.exit(1); }

const tmp = mkdtempSync(join(tmpdir(), 'praxi-docshots-'));
const started = Date.now();
let preview; let browser;
let failed = false;
const errors = [];
let written = 0;
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
    try { if ((await globalThis.fetch(`http://127.0.0.1:${port}/`)).ok) break; } catch { /* not up yet */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath: chromePath, headless: true, args: ['--no-sandbox'] });
  const quiet = '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}';

  const runCase = async (lang, screen) => {
    const context = await browser.newContext({ viewport: { ...VIEWPORT, height: screen.height ?? VIEWPORT.height }, reducedMotion: 'reduce', deviceScaleFactor: 1,
      locale: LANGUAGES[lang], timezoneId: TIME_ZONE });
    const page = await context.newPage();
    try {
      await page.clock.setFixedTime(NOW);
      const q = `mock=1&rt=ignore&lang=${lang}&theme=${THEME}&style=glass&motion=off${screen.query ? `&${screen.query}` : ''}`;
      await page.goto(`http://127.0.0.1:${port}/?${q}`, { waitUntil: 'load' });
      await page.waitForSelector('#root > *', { timeout: 15000 });
      await page.addStyleTag({ content: quiet });
      if (screen.ready) await page.waitForSelector(screen.ready, { timeout: 15000 });
      if (screen.act) await screen.act(page);
      await page.evaluate(() => document.fonts.ready);
      await page.waitForTimeout(700); // mock data settles
      // Jump script-driven animations (dialog and menu entrances) to their end state.
      await page.evaluate(() => document.getAnimations().forEach((a) => { try { a.finish(); } catch { /* endless */ } }));
      await page.waitForTimeout(200);
      const png = (await (screen.dialog ? page.locator('[role="dialog"]').first() : page).screenshot({ type: 'png' })).toString('base64');
      const webp = Buffer.from(await page.evaluate(toWebp, { png, quality }), 'base64');
      const target = join(outDir, lang, `${screen.name}.webp`);
      mkdirSync(dirname(target), { recursive: true });
      writeFileSync(target, webp);
      written++;
      process.stdout.write(`${lang}/${screen.name}.webp  ${(statSync(target).size / 1024).toFixed(0)} KB\n`);
    } catch (err) {
      errors.push(`${lang}/${screen.name}: ${err.message.split('\n')[0]}`);
    } finally { await context.close(); }
  };
  const cases = languages.flatMap((lang) => selected.map((screen) => [lang, screen]));
  let next = 0;
  await Promise.all(Array.from({ length: 3 }, async () => {
    while (next < cases.length) await runCase(...cases[next++]);
  }));

  const secs = ((Date.now() - started) / 1000).toFixed(0);
  process.stdout.write(`docs screenshots: ${written} written, ${errors.length} failed, ${secs}s -> ${outDir}\n`);
  for (const e of errors) process.stdout.write(`ERROR ${e}\n`);
  failed = errors.length > 0;
} finally {
  try { await browser?.close(); } catch { /* ignore */ }
  killTree(preview?.pid);
  rmSync(tmp, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
