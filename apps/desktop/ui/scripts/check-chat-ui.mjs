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
  const input = page.locator('[data-composer-input]');
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
  assert.equal(await page.getByRole('combobox').getAttribute('aria-expanded'), 'false');
  // One row under a message: icons on the left, the time right of them, at wide and narrow widths.
  await page.goto(base + '?mock=1&screen=answer-run&theme=obsidian&style=glass&wallpaper=sky&clear=1&veil=0&rt=ignore', { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.getByTestId('run-timeline').waitFor();
  for (const width of [1024, 480]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.waitForTimeout(350);
    const rows = await page.locator('[data-message-row="assistant"]').evaluateAll((nodes) => nodes.flatMap((node) => {
      const group = node.querySelector('[data-testid="message-actions"]');
      const time = node.querySelector('[data-testid="message-time"]');
      if (!group || !time) return [];
      const g = group.getBoundingClientRect(); const t = time.getBoundingClientRect();
      const bubble = node.querySelector('[data-message-body]')?.getBoundingClientRect();
      return [{ latest: group.getAttribute('data-latest') === 'true', gTop: g.top, gBottom: g.bottom, gRight: g.right, tTop: t.top, tBottom: t.bottom, tLeft: t.left, bubbleBottom: bubble?.bottom ?? 0 }];
    }));
    assert.ok(rows.length > 0, `${width}: no answer row with actions and time`);
    for (const row of rows) {
      assert.ok(row.tLeft >= row.gRight - 0.5, `${width}: time is not right of the icons`);
      assert.ok(row.tBottom > row.gTop && row.tTop < row.gBottom, `${width}: time and icons are not on one line`);
      assert.ok(row.gTop >= row.bubbleBottom - 0.5, `${width}: actions are not under the bubble`);
    }
    const latest = rows.filter((row) => row.latest);
    assert.equal(latest.length, 1, `${width}: the run actions belong in exactly one answer row`);
    assert.equal(await page.getByTestId('run-actions').count(), 0, `${width}: a second, stacked run bar is still rendered`);
    const mine = page.locator('[data-message-row="assistant"] [data-latest="true"]');
    for (const id of ['run-timeline']) assert.equal(await mine.getByTestId(id).count(), 1, `${width}: ${id} is not in the shared row`);
    await screenshot(page, `message-row-${width}`);
  }
  await page.setViewportSize({ width: 1024, height: 1000 });
  // Keyboard reaches the run actions, the copy button and the time in one tab sequence.
  await page.getByTestId('run-timeline').focus();
  await page.keyboard.press('Tab');
  assert.ok(await page.evaluate(() => document.activeElement?.closest('[data-testid="message-actions"]') !== null), 'Tab leaves the shared action row after one step');
  process.stdout.write('PASS message actions and time share one row, wide and narrow\n');

  // The light-model hint: nothing visible until hover, keyboard focus or a tap; Escape and an outside press close it.
  await page.goto(base + '?mock=1&litebot=1&panel=1&theme=obsidian&style=glass&wallpaper=sky&clear=1&veil=0&rt=ignore', { waitUntil: 'domcontentloaded', timeout: 120000 });
  const hint = page.getByTestId('bot-panel-lite-hint');
  const hintPanel = page.getByTestId('bot-panel-lite-hint-panel');
  await hint.waitFor();
  assert.equal(await hintPanel.isVisible(), false, 'the hint text must be hidden at rest');
  assert.equal(await page.locator('[data-testid="bot-panel"] [role="note"]').count(), 0, 'the permanent box is gone');
  const describedBy = await hint.getAttribute('aria-describedby');
  assert.ok((await page.locator(`[id="${describedBy}"]`).textContent()).length > 20, 'the hint text describes the icon');
  await hint.hover();
  await hintPanel.waitFor({ state: 'visible' });
  const inside = await page.evaluate(() => {
    const panel = document.querySelector('[data-testid="bot-panel-lite-hint-panel"]').getBoundingClientRect();
    const edge = document.querySelector('[data-hint-boundary]').getBoundingClientRect();
    return panel.left >= edge.left - 0.5 && panel.right <= edge.right + 0.5;
  });
  assert.ok(inside, 'the hint panel stays inside the bot panel header');
  await screenshot(page, 'model-hint-hover');
  await hintPanel.hover(); // the pointer can cross onto the panel (and its link) without it closing
  await page.waitForTimeout(400);
  assert.equal(await hintPanel.isVisible(), true, 'hint closes while the pointer is on it');
  await page.mouse.move(5, 5);
  await hintPanel.waitFor({ state: 'hidden' });
  await hint.focus();
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Tab');
  await hintPanel.waitFor({ state: 'visible' });
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute('data-testid')), 'bot-panel-lite-hint-change', 'Tab moves from the icon to "Change model"');
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Escape');
  await hintPanel.waitFor({ state: 'hidden' });
  await hint.click();
  await hintPanel.waitFor({ state: 'visible' });
  await page.mouse.click(5, 5);
  await hintPanel.waitFor({ state: 'hidden' });
  await hint.click();
  await page.getByTestId('bot-panel-lite-hint-change').click();
  await page.getByTestId('bot-panel-lite-hint-change').waitFor({ state: 'hidden' });
  await page.getByRole('dialog').first().waitFor();
  const touch = await browser.newContext({ hasTouch: true, isMobile: false, viewport: { width: 1024, height: 1000 } });
  const tap = await touch.newPage();
  await tap.goto(base + '?mock=1&litebot=1&panel=1&theme=obsidian&style=glass&wallpaper=sky&clear=1&veil=0&rt=ignore', { waitUntil: 'domcontentloaded', timeout: 120000 });
  await tap.getByTestId('bot-panel-lite-hint').tap();
  await tap.getByTestId('bot-panel-lite-hint-panel').waitFor({ state: 'visible' });
  await tap.getByTestId('bot-panel-lite-hint').tap();
  await tap.getByTestId('bot-panel-lite-hint-panel').waitFor({ state: 'hidden' });
  await touch.close();
  await page.addScriptTag({ path: resolve(root, 'node_modules/axe-core/axe.min.js') });
  const hintAudit = await page.evaluate(async () => (await window.axe.run(document.querySelector('[data-testid="bot-panel"]'))).violations.map((v) => `${v.id}: ${v.description}`));
  assert.deepEqual(hintAudit, []);
  process.stdout.write('PASS light-model hint: hidden at rest, hover, focus, tap, Escape, outside press, Change model\n');

  assert.deepEqual(errors, []);
  process.stdout.write('PASS avatars, grouped/recent commands, fuzzy highlights, keyboard, empty state and menu axe audit\n');
} finally {
  await browser?.close();
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(vite.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  else vite.kill();
}
