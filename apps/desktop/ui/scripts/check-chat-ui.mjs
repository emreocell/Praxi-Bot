/* global window, document */
// Focused real-DOM regressions for the wallpaper chat, composer menus and narrow header.
// Set CHAT_UI_ARTIFACTS (and TEMP/TMP) outside the worktree for browser output.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import process from 'node:process';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, URL } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../', import.meta.url));
const shots = process.env.CHAT_UI_ARTIFACTS;
if (shots) mkdirSync(shots, { recursive: true });
const executablePath = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  '/usr/bin/google-chrome', '/usr/bin/chromium'].find((p) => p && existsSync(p));
assert.ok(executablePath, 'Chrome is required; set CHROME_BIN');
const port = await new Promise((ok) => {
  const server = createServer();
  server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(() => ok(port)); });
});
const viteArgs = process.env.CHAT_UI_DIST ? ['preview', '--outDir', process.env.CHAT_UI_DIST] : [];
const vite = spawn(process.execPath, [resolve(root, 'node_modules/vite/bin/vite.js'), ...viteArgs, '--host', '127.0.0.1', '--port', String(port)],
  { cwd: root, stdio: 'ignore', windowsHide: true });
const base = `http://127.0.0.1:${port}`;
let browser;
const screenshot = async (page, name) => { if (shots) await page.screenshot({ path: resolve(shots, `${name}.png`) }); };
try {
  for (let i = 0; i < 100; i++) {
    try { if ((await globalThis.fetch(base)).ok) break; } catch { /* Vite is starting. */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 1024, height: 1000 } });
  page.setDefaultTimeout(60000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  const query = '?mock=1&screen=empty&theme=obsidian&style=glass&wallpaper=sky&clear=1&veil=0&rt=ignore';
  for (const host of ['', '&host=bots']) {
    await page.goto(base + query + host + '&lvState=paused', { waitUntil: 'domcontentloaded', timeout: 120000 });
    await page.getByTestId('bot-chip').waitFor();
    await page.getByTestId('paused-control-banner').waitFor();
    for (const width of [480, 640, 800, 1024]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.waitForTimeout(350);
      const header = page.locator('[class*="titleBar"]');
      const boxes = await header.evaluate((el) => [...el.children].map((c) => {
        const r = c.getBoundingClientRect(); return { left: r.left, right: r.right };
      }));
      assert.ok(boxes[0].right <= boxes[1].left + 0.5, `${width}: runtime collides with bot picker: ${JSON.stringify(boxes)}`);
      assert.ok(boxes[1].right <= boxes[2].left + 0.5, `${width}: bot collides with host picker: ${JSON.stringify(boxes)}`);
      for (const locator of [header, page.getByTestId('paused-control-banner'), page.locator('[class*="actions"]').filter({ has: page.getByTestId('paused-banner-stop') })]) {
        assert.equal(await locator.evaluate((el) => el.scrollWidth <= el.clientWidth + 1), true, `${width}: row overflows`);
      }
      const banner = page.getByTestId('paused-control-banner');
      const bannerBox = await banner.boundingBox();
      const buttons = await banner.locator('button').evaluateAll((nodes) => nodes.map((n) => {
        const r = n.getBoundingClientRect(); return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, overflow: n.scrollWidth > n.clientWidth + 1 };
      }));
      assert.ok(buttons.length >= 2);
      for (const button of buttons) {
        assert.ok(button.left >= bannerBox.x && button.right <= bannerBox.x + bannerBox.width, `${width}: button escapes banner`);
        assert.equal(button.overflow, false, `${width}: banner button clips text`);
      }
      for (let i = 0; i < buttons.length; i++) for (let j = i + 1; j < buttons.length; j++) {
        const a = buttons[i], b = buttons[j];
        assert.ok(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top, `${width}: banner buttons overlap`);
      }
      await screenshot(page, `${host ? 'host' : 'local'}-${width}`);
      process.stdout.write(`PASS ${host ? 'host' : 'local'} ${width}: header and paused banner fit\n`);
    }
  }
  await page.goto(base + query, { waitUntil: 'domcontentloaded', timeout: 120000 });
  const input = page.getByRole('combobox');
  await input.fill('@');
  const menu = page.getByTestId('composer-menu');
  await menu.waitFor();
  const sidebar = await page.locator('[class*="botList"] [data-bot-id]').evaluateAll((rows) => rows.map((row) => ({
    id: row.getAttribute('data-bot-id'), shape: row.querySelector('[data-shape]')?.getAttribute('data-shape')
  })));
  assert.ok(sidebar.length > 0);
  for (const bot of sidebar) {
    assert.equal(await menu.locator(`[data-bot-id="${bot.id}"] [data-shape]`).getAttribute('data-shape'), bot.shape, `${bot.id}: sidebar and mention avatar differ`);
  }
  await screenshot(page, 'mentions');
  await page.addScriptTag({ path: resolve(root, 'node_modules/axe-core/axe.min.js') });
  const audit = await page.evaluate(async () => {
    // Limit the audit to this task's popover; unrelated app violations do not mask menu failures.
    const result = await window.axe.run(document.querySelector('[data-testid="composer-menu"]'));
    return result.violations.map((v) => `${v.id}: ${v.description} (${v.nodes.map((n) => n.target.join(",")).join("; ")})`);
  });
  assert.deepEqual(audit, []);
  await input.fill('/stp');
  await menu.getByRole('option').first().waitFor();
  assert.equal(await menu.locator('mark').allTextContents().then((s) => s.join('')), 'stp');
  const selected = await input.getAttribute('aria-activedescendant');
  assert.equal(await page.locator(`[id="${selected}"]`).getAttribute('aria-selected'), 'true');
  await screenshot(page, 'fuzzy-command');
  await input.press('Enter');
  assert.equal(await input.inputValue(), '/stop ');
  await input.fill('/');
  assert.ok((await menu.getByRole('group').first().getAttribute('aria-label')).length > 0);
  assert.equal(await menu.getByRole('option').first().innerText().then((s) => s.split('\n')[0]), '/stop');
  await input.press('ArrowDown');
  assert.notEqual(await input.getAttribute('aria-activedescendant'), selected);
  await screenshot(page, 'commands-recent');
  await input.fill('/zzzzzz');
  await menu.getByRole('status').waitFor();
  assert.equal(await input.getAttribute('aria-activedescendant'), null);
  assert.equal(await menu.getByRole('option').count(), 0);
  await input.press('ArrowDown');
  await screenshot(page, 'commands-empty');
  await input.press('Escape');
  assert.equal(await input.getAttribute('aria-expanded'), 'false');
  assert.deepEqual(errors, []);
  process.stdout.write('PASS avatars, grouped/recent commands, fuzzy highlights, keyboard, empty state and menu axe audit\n');
} finally {
  await browser?.close();
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(vite.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  else vite.kill();
}
