/* global window, document, File, DataTransfer, ClipboardEvent, DragEvent */
// Real composer regression with fake uploads: no daemon, provider, credentials or network needed.
// Set TEMP/TMP and ATTACHMENT_ARTIFACTS to a disposable folder outside the worktree.
import assert from 'node:assert/strict';
import { Buffer } from 'node:buffer';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import process from 'node:process';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, URL } from 'node:url';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../', import.meta.url));
const artifacts = process.env.ATTACHMENT_ARTIFACTS;
if (artifacts) mkdirSync(artifacts, { recursive: true });
const executablePath = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  '/usr/bin/google-chrome', '/usr/bin/chromium'].find((p) => p && existsSync(p));
assert.ok(executablePath, 'Chrome is required; set CHROME_BIN');
const port = await new Promise((ok) => {
  const server = createServer();
  server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(() => ok(port)); });
});
const vite = spawn(process.execPath, [resolve(root, 'node_modules/vite/bin/vite.js'), '--host', '127.0.0.1', '--port', String(port)],
  { cwd: root, stdio: 'ignore', windowsHide: true });
const base = `http://127.0.0.1:${port}`;
let browser;
let page;
try {
  for (let i = 0; i < 100; i++) {
    try { if ((await globalThis.fetch(base)).ok) break; } catch { /* Vite is starting. */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath, headless: true });
  page = await browser.newPage({ viewport: { width: 1024, height: 1000 } });
  page.setDefaultTimeout(30000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  await page.route('**/*', (route) => route.request().url().startsWith(base) ? route.continue() : route.abort());
  await page.goto(base + '/?mock=1&screen=empty&theme=obsidian&rt=ignore&lang=en');
  const input = page.getByRole('combobox');
  await input.waitFor();
  await page.evaluate(async () => {
    const { ApiClient } = await import('/src/api/client.ts');
    window.attachmentCalls = [];
    window.attachmentMessages = [];
    ApiClient.prototype.uploadAttachment = async (_chat, blob, name) => {
      window.attachmentCalls.push({ name, size: blob.size, type: blob.type });
      await new Promise((resolve) => window.setTimeout(resolve, 300));
      return { id: name, kind: name.endsWith('.pdf') ? 'document' : name.endsWith('.txt') ? 'text' : 'image', name, size: blob.size, truncated: false };
    };
    ApiClient.prototype.sendMessage = async (_chat, _text, _mode, _key, ids) => {
      window.attachmentMessages.push(ids);
      return { delivery: 'started', run_id: null };
    };
  });
  // Native clipboard file payload: a real large bitmap exercises the browser encoder.
  await input.evaluate(async (el) => {
    const canvas = document.createElement('canvas');
    canvas.width = 3000; canvas.height = 1500;
    const context = canvas.getContext('2d');
    context.fillStyle = 'green'; context.fillRect(0, 0, canvas.width, canvas.height);
    const blob = await new Promise((ok) => canvas.toBlob(ok, 'image/png'));
    const data = new DataTransfer();
    data.items.add(new File([blob], 'pasted.png', { type: 'image/png' }));
    el.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: data }));
  });
  const list = page.getByRole('list', { name: 'Attached files' });
  await list.locator('img').waitFor();
  assert.equal(await list.locator('img').evaluate((img) => img.naturalWidth), 2048);
  assert.deepEqual(await page.evaluate(() => window.attachmentCalls), []);
  // File drop, including a PDF and a refused executable. Failed files must block Send.
  await input.evaluate((el) => {
    const data = new DataTransfer();
    data.items.add(new File(['%PDF-1.7\nfixture'], 'invoice.pdf', { type: 'application/pdf' }));
    data.items.add(new File(['MZ'], 'blocked.exe', { type: 'application/octet-stream' }));
    el.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: data }));
  });
  await list.locator('[data-state="failed"]').waitFor();
  assert.equal(await page.getByTestId('send-button').isDisabled(), true);
  const remove = page.getByRole('button', { name: 'Remove blocked.exe' });
  await remove.focus(); await remove.press('Enter');
  assert.equal(await input.evaluate((el) => el === document.activeElement), true);
  const chooser = page.waitForEvent('filechooser');
  const picker = page.getByRole('button', { name: 'Attach files or images', exact: true });
  await picker.focus(); await picker.press('Enter');
  await (await chooser).setFiles([{ name: 'picked.txt', mimeType: 'text/plain', buffer: Buffer.from('fixture text') }]);
  await list.getByText('picked.txt', { exact: true }).waitFor();
  await page.addScriptTag({ path: resolve(root, 'node_modules/axe-core/axe.min.js') });
  for (const lang of ['en', 'tr', 'de']) {
    await page.evaluate(async (lang) => { const { i18n } = await import('/src/i18n/index.ts'); await i18n.changeLanguage(lang); }, lang);
    for (const width of [360, 1024]) {
      await page.setViewportSize({ width, height: 1000 });
      const tray = page.locator('[data-no-composer-focus]').filter({ has: page.locator('ul[aria-live]') });
      assert.equal(await tray.evaluate((el) => el.scrollWidth <= el.clientWidth + 1), true, `${lang}/${width}: tray overflows`);
      const box = await tray.boundingBox();
      assert.ok(box.x >= 0 && box.x + box.width <= width, `${lang}/${width}: tray escapes viewport`);
      const violations = await tray.evaluate(async (el) => (await window.axe.run(el)).violations.map((v) => v.id));
      assert.deepEqual(violations, [], `${lang}/${width}: accessibility`);
      if (artifacts) await page.screenshot({ path: resolve(artifacts, `${lang}-${width}.png`) });
    }
  }
  await page.getByTestId('send-button').click();
  await page.locator('li[data-state="uploading"]').waitFor();
  await page.waitForFunction(() => window.attachmentMessages.length === 1);
  assert.deepEqual(await page.evaluate(() => window.attachmentMessages), [['pasted.png', 'invoice.pdf', 'picked.txt']]);
  assert.equal(await page.evaluate(() => window.attachmentCalls.length), 3);
  assert.deepEqual(errors, []);
  process.stdout.write('PASS: paste, drop, resized thumbnail, local-only draft, failed-file send guard, keyboard picker/removal, upload states, provider payload, en/tr/de at 360/1024px and axe\n');
} catch (error) {
  if (page) {
    process.stderr.write(await page.locator('footer').allTextContents().then((text) => text.join('\n')) + '\n');
    if (artifacts) await page.screenshot({ path: resolve(artifacts, 'failure.png') });
  }
  throw error;
} finally {
  await browser?.close();
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(vite.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  else vite.kill();
}
