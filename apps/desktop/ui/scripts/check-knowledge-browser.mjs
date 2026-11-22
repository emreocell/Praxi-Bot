// Exercise the actual knowledge card against the existing mock daemon in Chrome.
// Starts Vite and Chrome together so isolated test runners share one namespace.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL } from 'node:url';
import { Buffer } from 'node:buffer';
import { setTimeout as delay } from 'node:timers/promises';

const base = process.env.KNOWLEDGE_UI_URL ?? 'http://127.0.0.1:5181';
const output = resolve(process.env.KNOWLEDGE_UI_OUTPUT ?? '../../../validation/knowledge-browser');
const uiRoot = fileURLToPath(new URL('../', import.meta.url));
const vite = spawn(process.execPath, [join(uiRoot, 'node_modules/vite/bin/vite.js'), '--host', '127.0.0.1', '--port', new URL(base).port, '--strictPort'], { cwd: uiRoot, stdio: ['ignore', 'ignore', 'pipe'] });
let viteErrors = '';
vite.stderr.on('data', (data) => { viteErrors += data; });
const profile = await mkdtemp(join(tmpdir(), 'knowledge-browser-'));
const chrome = spawn(process.env.CHROME_BIN ?? '/usr/bin/chromium', ['--headless=new', '--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
let stderr = '';
chrome.stderr.on('data', (data) => { stderr += data; });
let socket;
const pending = new Map();
let seq = 0;
const failures = [];
const evidence = [];

try {
  let ready = false;
  for (let n = 0; n < 100; n++) {
    try { ready = (await globalThis.fetch(base)).ok; } catch { /* Vite is starting. */ }
    if (ready) break;
    await delay(100);
  }
  assert.ok(ready, `Vite did not start: ${viteErrors}`);
  for (let n = 0; n < 100 && !stderr.includes('DevTools listening on'); n++) await delay(100);
  const debug = /DevTools listening on (ws:\/\/[^\s]+)/.exec(stderr)?.[1];
  assert.ok(debug, `Chrome did not start: ${stderr.slice(-2000)}`);
  const origin = debug.replace(/^ws:/, 'http:').replace(/\/devtools\/.*$/, '');
  const pages = await (await globalThis.fetch(`${origin}/json/list`)).json();
  socket = new globalThis.WebSocket(pages.find((page) => page.type === 'page').webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  socket.onmessage = ({ data }) => {
    const value = JSON.parse(data);
    const waiter = pending.get(value.id);
    if (waiter) { pending.delete(value.id); if (value.error) waiter.reject(new Error(JSON.stringify(value.error))); else waiter.resolve(value.result); }
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++seq; pending.set(id, { resolve, reject }); socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result.value;
  };
  const waitFor = async (expression, label) => {
    for (let n = 0; n < 100; n++) { if (await evaluate(expression)) return; await delay(100); }
    throw new Error(`Timed out: ${label}`);
  };
  const check = async (name, run) => {
    try { await run(); evidence.push({ name, passed: true }); process.stdout.write(`PASS ${name}\n`); }
    catch (error) { failures.push(name); evidence.push({ name, passed: false, error: String(error) }); process.stdout.write(`FAIL ${name}: ${error}\n`); }
  };
  await mkdir(output, { recursive: true });
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 1365, height: 1100, deviceScaleFactor: 1, mobile: false });
  for (const theme of ['obsidian', 'clay', 'cobalt', 'porcelain']) {
    await send('Page.navigate', { url: `${base}/?mock=1&screen=settings&section=data&lang=en&theme=${theme}&style=solid` });
    await waitFor(`Array.from(document.querySelectorAll('h3')).some(e => e.textContent === 'Knowledge and shared brain')`, 'knowledge card');
    await waitFor(`Array.from(document.querySelectorAll('summary')).some(e => e.textContent === 'Add source')`, 'loaded source management');
    await evaluate(`globalThis.kcard = Array.from(document.querySelectorAll('h3')).filter(e => e.textContent === 'Knowledge and shared brain').at(-1).parentElement;
      globalThis.kfield = (text) => Array.from(kcard.querySelectorAll('label')).find(e => e.firstChild?.textContent.trim() === text)?.querySelector('input,select,textarea');
      globalThis.kset = (text, value) => { const el = kfield(text); if (!el) throw new Error('Missing field '+text); const prototype = el.tagName === 'SELECT' ? HTMLSelectElement.prototype : el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(prototype,'value').set.call(el,value); el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input',{bubbles:true})); };
      globalThis.kbutton = (text) => Array.from(kcard.querySelectorAll('button')).find(e => e.textContent.trim() === text);
      kcard.scrollIntoView({block:'start'}); Array.from(kcard.querySelectorAll('summary')).find(e => e.textContent === 'Add source').click();`);
    await waitFor(`Boolean(kfield('Source type'))`, 'source controls');
    await evaluate(`kset('Source type','shared_brain')`);
    await delay(100);
    await check(`${theme}: shared brain has a usable default name`, async () => {
      assert.equal(await evaluate(`kbutton('Add source').disabled`), false);
    });
    await evaluate(`kset('Source type','note')`);
    await delay(50);
    await evaluate(`kset('Source name','Review evidence'); kset('Content','Selected source evidence for the UI review.');`);
    await delay(50);
    await evaluate(`kbutton('Add source').click()`);
    await waitFor(`Array.from(kcard.querySelectorAll('strong')).some(e => e.textContent === 'Review evidence')`, 'created source');
    await check(`${theme}: source can be renamed`, async () => {
      assert.equal(await evaluate(`Boolean(kbutton('Rename'))`), true);
      await evaluate(`kbutton('Rename').click()`);
      await waitFor(`Boolean(kfield('New source name'))`, 'rename form');
      await evaluate(`kset('New source name','Renamed evidence')`);
      await delay(50);
      await evaluate(`kbutton('Save name').click()`);
      await waitFor(`Array.from(kcard.querySelectorAll('strong')).some(e => e.textContent === 'Renamed evidence')`, 'saved rename');
    });
    await check(`${theme}: remove requires a cancellable confirmation`, async () => {
      await evaluate(`kbutton('Remove from index').click()`);
      await delay(100);
      assert.equal(await evaluate(`Boolean(kcard.querySelector('li'))`), true, 'the first click must not delete the source');
      assert.equal(await evaluate(`Boolean(kbutton('Confirm removal'))`), true);
      await evaluate(`kbutton('Confirm removal').scrollIntoView({block:'center'})`);
      const removal = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
      await writeFile(join(output, `${theme}-remove.png`), Buffer.from(removal.data, 'base64'));
      await evaluate(`kbutton('Cancel').click()`);
      assert.equal(await evaluate(`Boolean(kcard.querySelector('li'))`), true);
    });
    await evaluate(`kcard.scrollIntoView({block:'start'})`);
    const metrics = await evaluate(`(() => { const r=kcard.getBoundingClientRect(); return { theme: document.documentElement.dataset.theme, card: {left:r.left,right:r.right,width:r.width}, overflow:kcard.scrollWidth-kcard.clientWidth, labels:Array.from(kcard.querySelectorAll('button')).map(b=>b.textContent.trim()) }; })()`);
    assert.ok(metrics.overflow <= 1, `${theme}: card horizontally overflows`);
    evidence.push({ name: `${theme}: rendered geometry`, ...metrics });
    const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
    await writeFile(join(output, `${theme}.png`), Buffer.from(shot.data, 'base64'));
    await evaluate(`Array.from(kcard.querySelectorAll('summary')).find(e=>e.textContent==='Retrieval settings').click()`);
    await check(`${theme}: automatic retrieval is visibly unavailable`, async () => {
      assert.equal(await evaluate(`Array.from(kcard.querySelectorAll('label')).find(e=>e.textContent.trim()==='Automatically retrieve relevant knowledge for new turns').querySelector('input').disabled`), true);
    });
    await evaluate(`Array.from(kcard.querySelectorAll('label')).find(e=>e.textContent.trim()==='Use an embedding provider').querySelector('input').click()`);
    await waitFor(`Boolean(kfield('Configured provider'))`, 'embedding provider');
    await evaluate(`kset('Configured provider','gemini')`);
    await delay(50);
    await evaluate(`kbutton('Save settings').click()`);
    await waitFor(`!kbutton('Allow document embeddings').disabled`, 'saved embedding profile');
    await check(`${theme}: document permission requires a server approval card`, async () => {
      await evaluate(`kbutton('Allow document embeddings').click()`);
      await waitFor(`Boolean(kbutton('Allow this source'))`, 'document permission review');
      assert.equal(await evaluate(`Boolean(kbutton('Revoke document embeddings'))`), false);
      assert.equal(await evaluate(`kcard.querySelector('[aria-label="Review embedding permission"]').textContent.includes('https://embedding.example.invalid')`), true);
      const approval = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
      await writeFile(join(output, `${theme}-approval.png`), Buffer.from(approval.data, 'base64'));
      await evaluate(`kbutton('Allow this source').click()`);
      await waitFor(`Boolean(kbutton('Revoke document embeddings'))`, 'confirmed document permission');
    });
    await evaluate(`Array.from(kcard.querySelectorAll('summary')).find(e=>e.textContent==='Retrieval settings').click()`);
    await check(`${theme}: query permission is reviewed independently`, async () => {
      assert.equal(await evaluate(`Boolean(kbutton('Revoke query permission'))`), false);
      await evaluate(`kbutton('Review query permission').click()`);
      await waitFor(`Boolean(kbutton('Allow retrieval queries'))`, 'query permission review');
      assert.equal(await evaluate(`Boolean(kbutton('Revoke query permission'))`), false);
      await evaluate(`kbutton('Allow retrieval queries').click()`);
      await waitFor(`Boolean(kbutton('Revoke query permission'))`, 'confirmed query permission');
      await evaluate(`Array.from(kcard.querySelectorAll('summary')).find(e=>e.textContent==='Retrieval settings').click()`);
      await evaluate(`kbutton('Revoke query permission').scrollIntoView({block:'center'})`);
      const settings = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
      await writeFile(join(output, `${theme}-settings.png`), Buffer.from(settings.data, 'base64'));
    });
    await check(`${theme}: local embedding selection exposes only installed profiles`, async () => {
      await evaluate(`kset('Configured provider','local-onnx')`);
      await waitFor(`Boolean(kfield('Installed local model'))`, 'installed model selector');
      assert.equal(await evaluate(`kfield('Installed local model').tagName`), 'SELECT');
      assert.equal(await evaluate(`kfield('Installed local model').options[1].value`), 'offline-e5');
      assert.equal(await evaluate(`kcard.textContent.includes('Local artifact manifest')`), false);
      await evaluate(`kfield('Installed local model').scrollIntoView({block:'center'})`);
      const local = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
      await writeFile(join(output, `${theme}-local-profile.png`), Buffer.from(local.data, 'base64'));
    });
    await check(`${theme}: failed email discovery leaves source management usable`, async () => {
      await evaluate(`(async () => { const {MockApiClient}=await import('/src/api/mock.ts'); MockApiClient.prototype.listKnowledgeEmailCaptures=async()=>{throw new Error('Email discovery fixture failure')}; kset('Bot',kfield('Bot').options[1].value); })()`);
      await delay(150);
      assert.equal(await evaluate(`Array.from(kcard.querySelectorAll('summary')).some(e=>e.textContent==='Retrieval settings')`), true);
    });
  }
} finally {
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'results.json'), JSON.stringify(evidence, null, 2));
  socket?.close();
  chrome.kill('SIGTERM');
  vite.kill('SIGTERM');
  await delay(100);
  await rm(profile, { recursive: true, force: true, maxRetries: 3 });
}
assert.equal(failures.length, 0, `${failures.length} knowledge UI regressions failed`);
