/* global window, document, CSS, requestAnimationFrame, performance */
// Run: node apps/desktop/ui/scripts/bench-long-chat.mjs
// Optional LONG_CHAT_DIST points to a VITE_MOCK=1 production build (recommended).
// Browser profiles and output belong under LONG_CHAT_ARTIFACTS, outside OneDrive.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import process from 'node:process';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, URL } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../', import.meta.url));
const artifacts = process.env.LONG_CHAT_ARTIFACTS;
assert.ok(artifacts, 'Set LONG_CHAT_ARTIFACTS to a throwaway directory outside the worktree');
mkdirSync(artifacts, { recursive: true });
const executablePath = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  '/usr/bin/google-chrome', '/usr/bin/chromium'].find((p) => p && existsSync(p));
assert.ok(executablePath, 'Chrome is required; set CHROME_BIN');
const port = await new Promise((ok) => {
  const server = createServer();
  server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(() => ok(port)); });
});
const args = process.env.LONG_CHAT_DIST ? ['preview', '--outDir', process.env.LONG_CHAT_DIST] : [];
const vite = spawn(process.execPath, [resolve(root, 'node_modules/vite/bin/vite.js'), ...args, '--host', '127.0.0.1', '--port', String(port)],
  { cwd: root, stdio: 'ignore', windowsHide: true });
const base = `http://127.0.0.1:${port}`;
let context;
try {
  for (let i = 0; i < 100; i++) {
    try { if ((await globalThis.fetch(base)).ok) break; } catch { /* Vite is starting. */ }
    await delay(200);
  }
  context = await chromium.launchPersistentContext(resolve(artifacts, 'profile'), {
    executablePath, headless: true, viewport: { width: 1280, height: 900 }, reducedMotion: 'reduce',
    args: ['--js-flags=--expose-gc', '--enable-precise-memory-info']
  });
  const page = await context.newPage();
  page.setDefaultTimeout(120000);
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`${base}/?mock=1&screen=long-chat&lang=en&rt=ignore`, { timeout: 120000 });
  const timeline = page.getByTestId('chat-timeline');
  await page.locator('#m-long-1999').waitFor({ state: 'attached' });
  await page.waitForTimeout(2500);
  assert.equal(await page.locator('[data-message-row]').count(), 2000);
  assert.equal(await page.getByTestId('code-block').count(), 1000);
  assert.equal(await page.getByTestId('thinking-chain').count(), 100);
  const session = await context.newCDPSession(page);
  await session.send('HeapProfiler.collectGarbage');
  const memory = await session.send('Runtime.getHeapUsage');
  const dom = await session.send('Memory.getDOMCounters');
  await session.send('Performance.enable');
  const beforeScroll = await session.send('Performance.getMetrics');
  const scroll = await timeline.evaluate(async (el) => {
    const frames = [];
    let previous = performance.now();
    // A local traversal avoids measuring a series of unrelated random jumps.
    el.scrollTop = 0;
    for (let i = 0; i < 120; i++) {
      await new Promise(requestAnimationFrame);
      const now = performance.now(); frames.push(now - previous); previous = now;
      el.scrollTop += 160;
    }
    frames.sort((a, b) => a - b);
    return { medianMs: frames[60], p95Ms: frames[114] };
  });
  const afterScroll = await session.send('Performance.getMetrics');
  for (const name of ['LayoutDuration', 'TaskDuration']) {
    const value = (metrics) => metrics.metrics.find((m) => m.name === name)?.value ?? 0;
    scroll[name + 'Ms'] = (value(afterScroll) - value(beforeScroll)) * 1000;
  }
  const input = page.locator('textarea').first();
  await input.focus();
  const typing = [];
  for (const character of 'Explain the latest result please') {
    const start = await page.evaluate(() => performance.now());
    await page.keyboard.insertText(character);
    const end = await page.evaluate(() => new Promise((ok) => requestAnimationFrame(() => ok(performance.now()))));
    typing.push(end - start);
  }
  typing.sort((a, b) => a - b);
  const result = {
    browser: context.browser()?.version(), messages: 2000, codeBlocks: 1000, stepGroups: 100,
    scroll, typing: { medianMs: typing[Math.floor(typing.length / 2)], p95Ms: typing[Math.floor(typing.length * 0.95)] },
    heapMiB: memory.usedSize / 1048576, domNodes: dom.nodes
  };
  const output = JSON.stringify(result, null, 2);
  writeFileSync(resolve(artifacts, 'metrics.json'), output + '\n');
  process.stdout.write(output + '\n');
  // Search must find offscreen text, and keyboard navigation and Jump to latest still work.
  await input.fill('');
  await page.keyboard.press('Control+f');
  const find = page.getByTestId('chat-find');
  await find.locator('input').fill('Search-marker-1.');
  await find.getByRole('status').filter({ hasText: /1/ }).waitFor();
  await find.locator('input').press('Enter');
  await find.locator('input').fill('value0');
  await find.getByRole('status').filter({ hasText: /1/ }).waitFor();
  await page.waitForTimeout(250);
  assert.ok(await page.evaluate(() => [...CSS.highlights.get('praxi-find')].every((r) => r.toString() === 'value0')), 'lazy code highlighting keeps search ranges current');
  await find.locator('input').press('Escape');
  const anchor = page.locator('#m-long-101');
  await anchor.evaluate((el) => el.scrollIntoView({ block: 'center' }));
  await page.waitForTimeout(500);
  const anchorY = await anchor.evaluate((el) => el.getBoundingClientRect().top);
  await page.locator('#m-long-1999').evaluate((el) => {
    const growth = document.createElement('p');
    growth.textContent = 'streamed text '.repeat(300);
    el.append(growth);
  });
  await page.waitForTimeout(250);
  assert.ok(Math.abs(await anchor.evaluate((el) => el.getBoundingClientRect().top) - anchorY) < 2, 'growth below leaves the reading position anchored');
  await timeline.focus();
  await timeline.press('Home');
  await timeline.evaluate((el) => { el.scrollTop = 0; el.dispatchEvent(new window.Event('scroll')); });
  await page.getByRole('button', { name: 'Jump to latest' }).click();
  await page.waitForTimeout(1000);
  assert.ok(await timeline.evaluate((el) => el.scrollHeight - el.clientHeight - el.scrollTop < 100), 'Jump follows measured content to the bottom');
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await timeline.evaluate((el) => { el.scrollTop = 0; el.dispatchEvent(new window.Event('scroll')); });
  await page.getByRole('button', { name: 'Jump to latest' }).click();
  await page.waitForFunction(() => {
    const el = document.querySelector('[data-testid="chat-timeline"]');
    return el && el.scrollHeight - el.clientHeight - el.scrollTop < 100;
  }, null, { timeout: 10000 });
  assert.equal(await timeline.getAttribute('role'), 'log');
  assert.equal(await timeline.getAttribute('aria-live'), 'off');
  await timeline.focus();
  await timeline.press('Home');
  await page.waitForTimeout(1000);
  await timeline.press('Tab');
  const focus = await timeline.evaluate((el) => ({
    within: el.contains(document.activeElement), tag: document.activeElement?.tagName,
    tabbable: document.activeElement !== el && document.activeElement?.tabIndex >= 0, top: el.scrollTop
  }));
  assert.ok(focus.within && focus.tabbable, `historical controls remain keyboard reachable: ${JSON.stringify(focus)}`);
  assert.deepEqual(errors, []);
  process.stdout.write('PASS offscreen/code search, scroll anchoring, keyboard focus, instant/smooth Jump to latest, log semantics, browser errors\n');
} finally {
  await context?.close();
  // This exact child directory is created by launchPersistentContext above.
  rmSync(resolve(artifacts, 'profile'), { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(vite.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  else vite.kill();
}
