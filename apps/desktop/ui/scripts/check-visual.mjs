/* global document, window, atob, Blob, createImageBitmap, OffscreenCanvas, getComputedStyle, NodeFilter */
// Visual regression + wallpaper text-contrast check over the mock-mode screens, in headless Chrome.
//   node scripts/check-visual.mjs [--update] [--only=<substring>] [--no-contrast] [--no-compare]
//
// Builds the mock bundle (VITE_MOCK=1), serves it with `vite preview` and opens every screen in the 4 themes
// (Obsidian, Cobalt, Raven, Clay) at 1280x800 and 640x800, on the still "sky" wallpaper with the default veil.
//   1. Compare: each screenshot is reduced to a 16x16 colour thumbnail; `tests/visual/baselines.json` stores that
//      thumbnail and a perceptual hash per screen (about 90 KB for everything, no PNG files in git).
//   2. Contrast: the text is hidden, the page is photographed again, and every text run is measured (WCAG AA, 4.5:1,
//      3:1 for large text) against the pixels behind it. Failures are listed, never allow-listed.
// `--update` rewrites the baselines.  VISUAL_OUT=<dir> keeps the screenshots and `report.json` (CI uploads them).
// Exit code 1 on any difference or contrast failure.  Chrome is found through CHROME_BIN.
import { Buffer } from 'node:buffer';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { compareThumb, dHash, encodeThumb, GRID, requiredRatio, worstContrast } from './visual-lib.mjs';

const args = process.argv.slice(2);
const flag = (name) => args.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const has = (name) => args.includes(`--${name}`);
const update = has('update');
const only = flag('only');
const doContrast = !has('no-contrast');
const doCompare = !has('no-compare');
const root = fileURLToPath(new URL('../', import.meta.url));
const baselinePath = resolve(root, 'tests/visual/baselines.json');
const outDir = process.env.VISUAL_OUT ? resolve(process.env.VISUAL_OUT) : null;
if (outDir) mkdirSync(outDir, { recursive: true });
const CHROMES = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium'].filter(Boolean);
const chromePath = CHROMES.find((p) => existsSync(p));
if (!chromePath) { process.stdout.write('check:visual needs Chrome (set CHROME_BIN)\n'); process.exit(1); }

const THEMES = ['obsidian', 'cobalt', 'raven', 'clay'];
const VIEWPORTS = [{ w: 1280, h: 800 }, { w: 640, h: 800 }];
const composerMenu = (text) => async (page) => {
  await page.getByRole('combobox').fill(text);
  await page.getByTestId('composer-menu').waitFor({ timeout: 15000 });
};
const SCREENS = [
  { name: 'chat-empty', query: 'screen=empty', ready: '[data-testid="bot-chip"]' },
  { name: 'chat-run', query: '', ready: '[data-testid="bot-chip"]' },
  { name: 'settings', query: 'screen=settings&section=general', ready: '[role="dialog"]' },
  { name: 'add-server', query: '', act: async (page) => {
    await page.getByTestId('computer-picker').click();
    await page.getByTestId('computer-add').click();
    await page.getByTestId('add-server').waitFor({ timeout: 15000 });
  } },
  { name: 'schedules', query: 'screen=schedules' },
  { name: 'approval', query: 'screen=approval', ready: '[data-testid^="approval"], [class*="approval"]' },
  { name: 'popover-command', query: 'screen=empty', act: composerMenu('/') },
  { name: 'popover-mention', query: 'screen=empty', act: composerMenu('@') }
];
const cases = [];
for (const s of SCREENS) for (const theme of THEMES) for (const v of VIEWPORTS) {
  cases.push({ ...s, theme, v, id: `${s.name}/${theme}/${v.w}x${v.h}` });
}
const selected = only ? cases.filter((c) => c.id.includes(only)) : cases;

const killTree = (pid) => {
  if (!pid) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  else { try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ } }
};
const freePort = () => new Promise((ok, fail) => {
  const s = createServer(); s.unref(); s.on('error', fail);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
});

// Runs in the page: collects every visible text run with its box, colour and size.
const collectText = () => {
  const runs = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = node.nodeValue.replace(/\s+/g, ' ').trim();
    const el = node.parentElement;
    if (!text || !el || el.closest('script,style,noscript,[hidden],[disabled],[aria-disabled="true"]')) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility !== 'visible' || cs.display === 'none') continue;
    let opacity = 1;
    for (let e = el; e; e = e.parentElement) opacity *= Number(getComputedStyle(e).opacity);
    const range = document.createRange(); range.selectNodeContents(node);
    const r = range.getBoundingClientRect();
    let x0 = Math.max(0, r.left); let y0 = Math.max(0, r.top);
    let x1 = Math.min(window.innerWidth, r.right); let y1 = Math.min(window.innerHeight, r.bottom);
    // Clipped text (overflow hidden, truncation) only counts where it is actually drawn.
    for (let a = el; a && a !== document.body; a = a.parentElement) {
      const o = getComputedStyle(a);
      if (o.overflowX === 'visible' && o.overflowY === 'visible') continue;
      const c = a.getBoundingClientRect();
      x0 = Math.max(x0, c.left); y0 = Math.max(y0, c.top); x1 = Math.min(x1, c.right); y1 = Math.min(y1, c.bottom);
    }
    if (x1 - x0 < 2 || y1 - y0 < 2 || opacity < 0.05) continue;
    // Text covered by a dialog or popover is not on screen: the element under the middle of the run must be its own.
    const hit = document.elementFromPoint((x0 + x1) / 2, (y0 + y1) / 2);
    if (!hit || !(el.contains(hit) || hit.contains(el))) continue;
    const m = /rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/.exec(cs.color);
    if (!m) continue;
    const cls = typeof el.className === 'string' ? el.className.split(/\s+/)[0] : '';
    runs.push({ text: text.slice(0, 48), box: [x0, y0, x1, y1].map(Math.round), fg: [+m[1], +m[2], +m[3]],
      alpha: (m[4] === undefined ? 1 : +m[4]) * opacity, font: parseFloat(cs.fontSize), weight: Number(cs.fontWeight) || 400,
      where: `${el.tagName.toLowerCase()}${cls ? `.${cls}` : ''}` });
  }
  return runs;
};
// Runs in the page: decodes a PNG (base64) and returns the 16x16 thumbnail, plus the pixels under each text box.
const analyse = async ({ shot, bgShot, runs, grid }) => {
  const load = async (b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    const bmp = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
    const ctx = new OffscreenCanvas(bmp.width, bmp.height).getContext('2d', { willReadFrequently: true });
    ctx.drawImage(bmp, 0, 0);
    return ctx.getImageData(0, 0, bmp.width, bmp.height);
  };
  const img = await load(shot);
  const thumb = [];
  for (let gy = 0; gy < grid; gy++) for (let gx = 0; gx < grid; gx++) {
    const x0 = Math.floor((gx * img.width) / grid); const x1 = Math.floor(((gx + 1) * img.width) / grid);
    const y0 = Math.floor((gy * img.height) / grid); const y1 = Math.floor(((gy + 1) * img.height) / grid);
    const sum = [0, 0, 0]; let n = 0;
    for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) {
      const i = (y * img.width + x) * 4; sum[0] += img.data[i]; sum[1] += img.data[i + 1]; sum[2] += img.data[i + 2]; n++;
    }
    thumb.push(Math.round(sum[0] / n), Math.round(sum[1] / n), Math.round(sum[2] / n));
  }
  const samples = [];
  if (bgShot) {
    const bg = await load(bgShot);
    for (const run of runs) {
      const [x0, y0, x1, y1] = run.box; const px = [];
      const step = Math.max(1, Math.floor(Math.sqrt(((x1 - x0) * (y1 - y0)) / 300)));
      for (let y = y0; y < y1; y += step) for (let x = x0; x < x1; x += step) {
        const i = (y * bg.width + x) * 4; px.push([bg.data[i], bg.data[i + 1], bg.data[i + 2]]);
      }
      samples.push(px);
    }
  }
  return { thumb, samples };
};

const tmp = mkdtempSync(join(tmpdir(), 'praxi-visual-'));
const started = Date.now();
let preview; let browser;
let failed = false;
const baselines = existsSync(baselinePath) ? JSON.parse(readFileSync(baselinePath, 'utf8')) : {};
const fresh = {};
const diffs = [];
const lowContrast = [];
const errors = [];
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
  const hideText = '*,*::before,*::after{color:transparent!important;text-shadow:none!important;-webkit-text-fill-color:transparent!important}input::placeholder,textarea::placeholder{color:transparent!important}';

  const runCase = async (c) => {
    const context = await browser.newContext({ viewport: { width: c.v.w, height: c.v.h }, reducedMotion: 'reduce', deviceScaleFactor: 1, locale: 'en-US' });
    const page = await context.newPage();
    try {
      // Fixed state: English, the still sky wallpaper behind a see-through chat area, default veil, glass panels.
      const q = `mock=1&rt=ignore&lang=en&theme=${c.theme}&style=glass&wallpaper=sky&motion=off&clear=1${c.query ? `&${c.query}` : ''}`;
      await page.goto(`http://127.0.0.1:${port}/?${q}`, { waitUntil: 'load' });
      await page.waitForSelector('#root > *', { timeout: 15000 });
      await page.addStyleTag({ content: quiet });
      if (c.ready) await page.waitForSelector(c.ready, { timeout: 15000 });
      if (c.act) await c.act(page);
      await page.evaluate(() => document.fonts.ready);
      await page.waitForTimeout(700); // mock data settles
      const shot = (await page.screenshot({ type: 'png' })).toString('base64');
      let runs = []; let bgShot = null;
      if (doContrast) {
        runs = await page.evaluate(collectText);
        const style = await page.addStyleTag({ content: hideText });
        await page.waitForTimeout(100);
        bgShot = (await page.screenshot({ type: 'png' })).toString('base64');
        await style.evaluate((el) => el.remove());
      }
      const { thumb, samples } = await page.evaluate(analyse, { shot, bgShot, runs, grid: GRID });
      if (outDir) writeFileSync(join(outDir, `${c.id.replaceAll('/', '_')}.png`), Buffer.from(shot, 'base64'));
      fresh[c.id] = { hash: dHash(thumb), thumb: encodeThumb(thumb) };
      if (doContrast) {
        runs.forEach((run, i) => {
          const need = requiredRatio(run.font, run.weight);
          const got = worstContrast(run.fg, run.alpha, samples[i]);
          if (got < need) lowContrast.push({ screen: c.id, box: run.box, text: run.text, where: run.where, ratio: Number(got.toFixed(2)), need, font: run.font });
        });
      }
      if (doCompare && !update) {
        const base = baselines[c.id];
        if (!base) diffs.push({ screen: c.id, reasons: ['no baseline yet (run with --update)'] });
        else {
          const r = compareThumb(thumb, base);
          if (!r.ok) diffs.push({ screen: c.id, reasons: r.reasons });
        }
      }
    } catch (err) {
      errors.push(`${c.id}: ${err.message.split('\n')[0]}`);
    } finally { await context.close(); }
  };
  let next = 0;
  await Promise.all(Array.from({ length: 3 }, async () => {
    while (next < selected.length) await runCase(selected[next++]);
  }));

  const secs = ((Date.now() - started) / 1000).toFixed(0);
  process.stdout.write(`visual: ${selected.length} screens (${THEMES.length} themes x ${VIEWPORTS.length} sizes), ${secs}s\n`);
  for (const e of errors) process.stdout.write(`ERROR ${e}\n`);
  if (update) {
    const keep = only ? { ...baselines } : {};
    const sorted = Object.fromEntries(Object.entries({ ...keep, ...fresh }).sort(([a], [b]) => a.localeCompare(b)));
    mkdirSync(dirname(baselinePath), { recursive: true });
    writeFileSync(baselinePath, `${JSON.stringify(sorted).replaceAll('},"', '},\n"')}\n`);
    process.stdout.write(`baselines written: ${Object.keys(sorted).length} screens -> tests/visual/baselines.json\n`);
  }
  for (const d of diffs) process.stdout.write(`DIFF ${d.screen}: ${d.reasons.join('; ')}\n`);
  // Group contrast failures so a long list stays readable; the full list is in report.json.
  const byScreen = new Map();
  for (const f of lowContrast) byScreen.set(f.screen, [...(byScreen.get(f.screen) ?? []), f]);
  for (const [screen, list] of [...byScreen].sort(([a], [b]) => a.localeCompare(b))) {
    process.stdout.write(`LOW CONTRAST ${screen}: ${list.length} text runs below WCAG AA\n`);
    for (const f of list.sort((a, b) => a.ratio - b.ratio).slice(0, 4)) process.stdout.write(`  ${f.ratio}:1 (needs ${f.need}:1) "${f.text}" in ${f.where}\n`);
  }
  if (outDir) writeFileSync(join(outDir, 'report.json'), JSON.stringify({ diffs, lowContrast, errors }, null, 1));
  if (diffs.length || lowContrast.length || errors.length) failed = true;
  if (!failed) process.stdout.write('visual OK\n');
  else process.stdout.write(`visual: ${diffs.length} differences, ${lowContrast.length} low-contrast text runs, ${errors.length} errors\n`);
} finally {
  try { await browser?.close(); } catch { /* ignore */ }
  killTree(preview?.pid);
  rmSync(tmp, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
