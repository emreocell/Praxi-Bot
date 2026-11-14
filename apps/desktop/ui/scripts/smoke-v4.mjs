// Browser smoke review against the running mock server; no browser package required.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import process from 'node:process';
import { setTimeout as delay } from 'node:timers/promises';

const evidence = resolve(process.argv[2] ?? '../../../../design/v4');
mkdirSync(evidence, { recursive: true });
const chrome = spawn('C:/Program Files/Google/Chrome/Application/chrome.exe', [
  '--headless=new', '--disable-gpu', '--no-sandbox', '--remote-debugging-port=9335',
  `--user-data-dir=${resolve(evidence, 'smoke-profile')}`, 'about:blank'
], { windowsHide: true, stdio: 'ignore' });
const results = [];
const errors = [];
let ws;
try {
  let tabs;
  for (let n = 0; n < 50; n++) {
    try { tabs = await (await globalThis.fetch('http://127.0.0.1:9335/json/list')).json(); break; }
    catch { await delay(100); }
  }
  assert.ok(tabs?.length, 'Chrome debugging ready');
  ws = new globalThis.WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(resolve => ws.addEventListener('open', resolve, { once: true }));
  const pending = new Map();
  let counter = 0;
  ws.addEventListener('message', ({ data }) => {
    const value = JSON.parse(data);
    if (value.id) { pending.get(value.id)?.(value); pending.delete(value.id); }
    if (value.method === 'Runtime.exceptionThrown') errors.push(value.params.exceptionDetails.text);
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++counter;
    pending.set(id, result => result.error ? reject(new Error(JSON.stringify(result.error))) : resolve(result.result));
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async expression => {
    const r = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };
  const check = async (label, expression) => {
    assert.ok(await evaluate(expression), label);
    results.push(label);
    process.stdout.write(`PASS ${label}\n`);
  };
  const navigate = async query => {
    await call('Page.navigate', { url: `http://localhost:5182/?mock=1&${query}` });
    await delay(1200);
    await evaluate('document.fonts.ready');
  };
  await call('Runtime.enable');
  await call('Emulation.setDeviceMetricsOverride', { width: 1600, height: 1000, deviceScaleFactor: 1, mobile: false });
  await navigate('theme=obsidian');
  await check('four bot characters and rich conversation', 'document.querySelectorAll("aside [data-state]").length === 5 && !!document.querySelector("table") && !!document.querySelector("[data-testid=code-block]") && !!document.querySelector("[data-testid=tool-card]")');
  await check('normal UI hides connected/provider/dev badges', '!document.body.innerText.includes("Daemon bağlı") && !document.body.innerText.includes("Sahte sağlayıcı") && !document.body.innerText.includes("Bot çalışmasını simüle et")');
  await check('icon buttons expose labels and tooltips', '[...document.querySelectorAll("button")].filter(b => b.children.length && !b.innerText.trim() && b.querySelector("svg") && !b.querySelector("[role=img]")).every(b => b.getAttribute("aria-label") && b.title)');
  await evaluate('document.querySelector("[data-testid=account-menu]").click()'); await delay(100);
  await check('schedules reachable from account menu', '!!document.querySelector("[data-testid=account-schedules]")');
  await evaluate('document.querySelector("[data-testid=account-schedules]").click()'); await delay(150);
  await check('schedules route renders', 'document.body.innerText.includes("Zamanlanmış görevler")');
  await navigate('theme=obsidian&screen=models');
  await check('five provider groups with brand marks', 'document.querySelectorAll("[role=listbox] [role=group]").length === 5 && document.querySelector("[role=listbox]").innerText.includes("OpenAI")');
  await evaluate('[...document.querySelectorAll("[role=option]")].find(b=>b.innerText.includes("gpt-4.1")).click()'); await delay(250);
  await check('model choice persists through profile update', 'document.querySelector("[data-testid=model-picker]").innerText.includes("gpt-4.1")');
  await navigate('theme=clay&screen=settings');
  await check('settings grouped', 'document.querySelector("[role=dialog]").innerText.includes("Davranış")');
  await check('modal makes background inert', 'document.getElementById("root").inert');
  await evaluate('document.querySelector("[data-testid=settings-nav-appearance]").click()'); await delay(150);
  await check('appearance shows the five favourite themes', 'document.querySelectorAll("[role=dialog] [data-theme-preview]").length === 5');
  await evaluate('document.querySelector("[role=dialog] [aria-haspopup=dialog]").click()'); await delay(300);
  await check('plus tile opens the themes modal with every theme', 'document.querySelectorAll("[role=dialog][aria-label=Temalar] [data-theme-preview]").length === 29');
  await evaluate('[...document.querySelectorAll("[data-theme-preview]")].find(n=>n.dataset.themePreview==="raven").closest("button").click()'); await delay(100);
  await check('theme tile updates active theme', 'document.documentElement.dataset.theme === "raven"');
  await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape' }); await delay(150);
  await check('Escape closes only the themes modal', 'document.querySelectorAll("[role=dialog]").length === 1 && document.getElementById("root").inert');
  await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape' }); await delay(100);
  await check('Escape closes modal and restores app', '!document.querySelector("[role=dialog]") && !document.getElementById("root").inert');
  await navigate('theme=clay&screen=apps');
  await check('connected apps show colored logos and mini characters', 'document.querySelectorAll("[data-testid^=mcp-server-]").length === 6 && document.querySelectorAll("[data-testid^=mcp-server-] [data-state]").length > 5 && [...document.images].filter(i=>i.getAttribute("src").startsWith("/brands/")).length >= 2');
  await check('catalog is long and searchable', 'document.querySelectorAll("[data-testid^=mcp-catalog-]").length >= 20');
  await evaluate('(()=>{const e=document.querySelector("input[type=search]");Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").set.call(e,"Notion");e.dispatchEvent(new Event("input",{bubbles:true}));})()'); await delay(100);
  await check('app filtering works', 'document.querySelectorAll("[data-testid^=mcp-catalog-]").length === 1 && !!document.querySelector("[data-testid=mcp-catalog-notion]")');
  await navigate('theme=obsidian&screen=empty');
  await evaluate('(()=>{const e=document.querySelector("textarea");Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,"value").set.call(e,"Smoke review message");e.dispatchEvent(new Event("input",{bubbles:true}));})()'); await delay(100);
  await evaluate('document.querySelector("button[aria-label=Gönder]").click()'); await delay(200);
  await check('composer sends through mock API', 'document.querySelector("[data-testid=chat-timeline]").innerText.includes("Smoke review message") && document.querySelector("textarea").value === ""');
  await call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
  await check('reduced motion disables character animations', '[...document.querySelectorAll("[data-state] svg")].every(n => getComputedStyle(n).animationName === "none")');
  await call('Emulation.setDeviceMetricsOverride', { width: 1000, height: 760, deviceScaleFactor: 1, mobile: false });
  await check('resized desktop has no document overflow', 'document.documentElement.scrollWidth <= innerWidth');
  await check('no replacement characters in localized UI', '!document.body.innerText.includes("\uFFFD")');
  assert.equal(errors.length, 0, 'no uncaught browser errors');
  results.push('no uncaught browser errors');
} finally {
  writeFileSync(resolve(evidence, 'smoke-results.json'), JSON.stringify({ results, errors }, null, 2));
  ws?.close();
  chrome.kill();
}
