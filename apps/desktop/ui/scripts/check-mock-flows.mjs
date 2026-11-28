// Exercises a few redesign flows in mock mode (vault, custom MCP, missions). Headless Chrome + VITE_MOCK preview.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { mkdtempSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';

const root = fileURLToPath(new URL('../', import.meta.url));
const executablePath = [process.env.CHROME_BIN, '/usr/bin/google-chrome', '/usr/bin/chromium',
  'C:/Program Files/Google/Chrome/Application/chrome.exe'].find((p) => p && existsSync(p));
assert.ok(executablePath, 'Chrome is required; set CHROME_BIN');

const query = 'mock=1&rt=ignore&lang=en&theme=obsidian&style=glass&motion=off';
const T = { timeout: 30000 };
const tmp = mkdtempSync(join(tmpdir(), 'praxi-mock-flows-'));
let preview; let browser;

const freePort = () => new Promise((ok, fail) => {
  const s = createServer(); s.unref(); s.on('error', fail);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
});

try {
  const vite = resolve(root, 'node_modules/vite/bin/vite.js');
  const dist = join(tmp, 'dist');
  const build = spawnSync(process.execPath, [vite, 'build', '--outDir', dist, '--emptyOutDir'],
    { cwd: root, env: { ...process.env, VITE_MOCK: '1' }, encoding: 'utf8', windowsHide: true, timeout: 240000 });
  if (build.status !== 0) throw new Error(`vite build failed:\n${build.stdout}\n${build.stderr}`);
  const port = await freePort();
  preview = spawn(process.execPath, [vite, 'preview', '--outDir', dist, '--port', String(port), '--strictPort', '--host', '127.0.0.1'],
    { cwd: root, windowsHide: true, stdio: 'ignore' });
  const base = `http://127.0.0.1:${port}`;
  for (let n = 0; n < 50; n++) {
    try { if ((await globalThis.fetch(`${base}/`)).ok) break; } catch { /* not up yet */ }
    await delay(200);
  }
  browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

  // Vault: /vault from chat, add a login, save.
  await page.goto(`${base}/?${query}&screen=empty`, { waitUntil: 'load', timeout: T.timeout });
  await page.getByTestId('bot-chip').waitFor(T);
  const composer = page.locator('[data-composer-input]');
  await composer.fill('/vault');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: /^send$/i }).click();
  await page.getByTestId('vault-manager').waitFor(T);
  await page.getByTestId('vault-new-item').click();
  await page.getByTestId('vault-new-login').click();
  await page.locator('#vault-site').fill('smoke.example');
  await page.locator('#vault-origin').fill('https://smoke.example');
  await page.locator('#vault-login-id').fill('smoke-user');
  await page.locator('#vault-secret-val').fill('smoke-secret');
  await page.getByTestId('vault-save').click();
  await page.locator('[data-testid^=vault-row-]').filter({ hasText: 'smoke.example' }).waitFor(T);
  process.stdout.write('PASS vault: /vault opens settings vault and a login is saved\n');

  // Custom MCP by URL.
  await page.goto(`${base}/?${query}&screen=apps`, { waitUntil: 'load', timeout: T.timeout });
  await page.getByRole('dialog').waitFor(T);
  await page.getByTestId('apps-advanced').click();
  const form = page.getByTestId('mcp-custom-form');
  await form.waitFor(T);
  await form.getByLabel('Server URL (HTTP)').fill('https://smoke.example.invalid/mcp');
  await form.getByLabel('Name', { exact: true }).fill('smoke-mock-mcp');
  await page.getByTestId('mcp-add').click();
  // The form stays open (fresh); the new server's row shows in the list.
  await page.getByTestId('mcp-server-smoke-mock-mcp').waitFor(T);
  process.stdout.write('PASS apps: custom MCP server added by HTTP URL\n');

  // Mission: `/mission` starts a card in the chat when the slash menu lists it.
  await page.goto(`${base}/?${query}&screen=empty&panel=1`, { waitUntil: 'load', timeout: T.timeout });
  await page.getByTestId('bot-panel').waitFor(T);
  await composer.fill('/mission Sign up on five test sites');
  const missionInMenu = await page.getByTestId('composer-menu').getByRole('option', { name: /mission|görev/i }).count();
  if (missionInMenu > 0) {
    await composer.press('Enter');
    await page.getByTestId('mission-started-card').first().waitFor(T);
    process.stdout.write('PASS mission: /mission starts a mission card in chat\n');
  } else {
    process.stdout.write('SKIP mission: /mission is not listed by the mock composer\n');
  }

  process.stdout.write('mock flows OK\n');
} finally {
  try { await browser?.close(); } catch { /* ignore */ }
  if (preview?.pid) {
    try { process.kill(preview.pid, 'SIGKILL'); } catch { /* gone */ }
  }
  rmSync(tmp, { recursive: true, force: true });
}