// Real-app smoke run (card 263): the REAL UI (vite dev, non-mock) against a REAL local daemon
// (`bot serve`, debug build) that talks to a tiny scripted OpenAI-compatible stub. Local and
// optional, not part of CI.
//   node scripts/smoke.mjs [--no-build] [--only=<substring>] [--keep] [--headed]
// Everything it starts is stopped by PID in `finally`; its temp dir is deleted unless --keep.
//
// Tauri commands shimmed for the browser run (everything else answers `null` and is listed at the end):
//   daemon_connect / daemon_restart  -> the daemon this script started (started_by_app: true)
//   owner_key                        -> the owner key handed to `bot serve --owner-key-stdin`
//   providers_cli                    -> runs the real `bot providers ...` (API base URLs of the
//                                       well-known providers are pointed at the stub; no network)
//   get_autostart / autostart_supported / set_autostart, is_human_driving
//   get_control_state and the other stop/pause/takeover commands -> an idle control snapshot
//   plugin:store|*                   -> rejected, so settings fall back to localStorage (as in a browser)
/* global fetch, URL, setTimeout, clearTimeout */
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright-core';
import { flows } from './smoke.spec.mjs';

const args = process.argv.slice(2);
const flag = (name) => args.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const root = fileURLToPath(new URL('../', import.meta.url));
const repo = resolve(root, '../../..');
const shots = process.env.SMOKE_SHOTS ?? join(tmpdir(), 'praxi-smoke-shots');
const targetDir = process.env.CARGO_TARGET_DIR ?? join(repo, 'target');
const exe = process.platform === 'win32' ? 'bot.exe' : 'bot';
const bot = join(targetDir, 'debug', exe);
const CHROMES = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium'].filter(Boolean);
const chromePath = CHROMES.find((p) => existsSync(p));

const children = [];
const log = (m) => process.stdout.write(`${m}\n`);
const freePort = () => new Promise((ok, no) => {
  const s = createServer();
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
  s.on('error', no);
});
const killTree = (c) => {
  if (!c.pid) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(c.pid), '/T', '/F'], { stdio: 'ignore' });
  else try { process.kill(-c.pid); } catch { /* gone */ }
};
const waitFor = async (what, fn, ms = 60000) => {
  const end = Date.now() + ms;
  for (;;) {
    try { const v = await fn(); if (v) return v; } catch { /* retry */ }
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await delay(300);
  }
};

// ------------------------------------------------------------------ scripted OpenAI-compatible stub
/** What the shell answers for the stop / control state while nothing is running. */
const CONTROL = { mode: 'idle', runGeneration: 0, bot: null, currentStep: null, stopHotkey: 'Ctrl+Alt+Esc', activeTarget: null, lastClick: null, refusedActionsCount: 0, stateRev: 1, stopSeq: 0 };
const MODELS = ['stub-small', 'stub-large', 'stub-mini'];
const stubLog = [];
function scripted(messages) {
  const lastUser = [...messages].map((m, i) => [m, i]).reverse().find(([m]) => m.role === 'user');
  const text = typeof lastUser?.[0].content === 'string' ? lastUser[0].content
    : (lastUser?.[0].content ?? []).map((p) => p.text ?? '').join('');
  const lines = text.split('\n').map((l) => l.trim()).filter(Boolean);
  const calls = lines.length > 0 && lines.every((l) => l.startsWith('call:'))
    ? lines.map((l) => { const r = l.slice(5); const i = r.indexOf(' '); return i < 0 ? [r, '{}'] : [r.slice(0, i), r.slice(i + 1)]; }) : null;
  // `slow:<ms>`: the model starts answering and then stalls (Stop cases hold the provider request open).
  const slow = /^slow:(\d+)/.exec(text.trim());
  if (slow) return { slow: Number(slow[1]) };
  const done = messages.slice((lastUser?.[1] ?? 0) + 1).filter((m) => m.role === 'tool').length;
  if (calls && done < calls.length) return { call: { name: calls[done][0], args: calls[done][1], id: `call_${done + 1}` } };
  const last = messages.at(-1);
  if (last?.role === 'tool') return { text: `tool said: ${String(last.content).slice(0, 200)}` };
  return { text: `echo: ${text.replace(/\s+/g, ' ').trim().slice(0, 300)}` };
}
function startStub(port) {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (d) => { body += d; });
    req.on('end', () => {
      stubLog.push(`${req.method} ${req.url}`);
      const path = req.url.split('?')[0];
      if (req.method === 'GET' && path === '/search') {
        // SearXNG-shaped answer: the one offline source of "untrusted content" (a search result).
        res.setHeader('content-type', 'application/json');
        res.end(JSON.stringify({ results: [{ title: 'Smoke result', url: 'https://example.org/smoke', content: 'untrusted page text from the stub' }] }));
        return;
      }
      if (req.method === 'GET' && path.endsWith('/models')) {
        res.setHeader('content-type', 'application/json');
        res.end(JSON.stringify({ object: 'list', data: MODELS.map((id) => ({ id, object: 'model', owned_by: 'stub' })) }));
        return;
      }
      if (req.method === 'POST' && path.endsWith('/chat/completions')) {
        const payload = JSON.parse(body || '{}');
        const out = scripted(payload.messages ?? []);
        const chunk = (delta, finish = null) => `data: ${JSON.stringify({ id: 'stub', object: 'chat.completion.chunk', model: payload.model, choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
        if (payload.stream === false) {
          res.setHeader('content-type', 'application/json');
          res.end(JSON.stringify({ id: 'stub', object: 'chat.completion', model: payload.model, choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: out.text ?? 'ok' } }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } }));
          return;
        }
        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
        res.write(chunk({ role: 'assistant', content: '' }));
        if (out.slow) {
          stubLog.push('MODEL_OPEN');
          res.write(chunk({ content: 'thinking ' }));
          const timer = setTimeout(() => { res.write(chunk({ content: 'done' })); res.write(chunk({}, 'stop')); res.end('data: [DONE]\n\n'); }, out.slow);
          res.on('close', () => { clearTimeout(timer); if (!res.writableEnded) stubLog.push('MODEL_ABORTED'); });
          return;
        }
        if (out.call) {
          res.write(chunk({ tool_calls: [{ index: 0, id: out.call.id, type: 'function', function: { name: out.call.name, arguments: out.call.args } }] }));
          res.write(chunk({}, 'tool_calls'));
        } else {
          for (const w of out.text.split(/(?<= )/)) res.write(chunk({ content: w }));
          res.write(chunk({}, 'stop'));
        }
        res.write(`data: ${JSON.stringify({ id: 'stub', object: 'chat.completion.chunk', model: payload.model, choices: [], usage: { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 } })}\n\n`);
        res.end('data: [DONE]\n\n');
        return;
      }
      res.statusCode = 404;
      res.end('{}');
    });
  });
  return new Promise((ok) => server.listen(port, '127.0.0.1', () => ok(server)));
}

// ------------------------------------------------------------------ main
async function main() {
  if (!chromePath) throw new Error('no Chrome found (set CHROME_BIN)');
  if (!args.includes('--no-build')) {
    log('building `bot` (debug)...');
    const b = spawnSync('cargo', ['build', '-p', 'cli'], { cwd: repo, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: targetDir, CARGO_INCREMENTAL: '0', CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '3' } });
    if (b.status !== 0) throw new Error('cargo build failed');
    // The hermetic fake coding CLI of the native-tools tests; the run installs it as `cursor-agent` (below).
    const f = spawnSync('cargo', ['build', '-p', 'cli-provider', '--bin', 'native-cli-fixture'], { cwd: repo, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: targetDir, CARGO_INCREMENTAL: '0', CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '3' } });
    if (f.status !== 0) throw new Error('cargo build of the fake CLI failed');
  }
  if (!existsSync(bot)) throw new Error(`missing ${bot}`);
  const tmp = mkdtempSync(join(tmpdir(), 'praxi-smoke-'));
  const dataDir = join(tmp, 'data');
  const workDir = join(tmp, 'work');
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(workDir, { recursive: true });
  writeFileSync(join(workDir, 'hello.txt'), 'hello from the smoke run\n');
  mkdirSync(shots, { recursive: true });
  const [stubPort, daemonPort, uiPort] = [await freePort(), await freePort(), await freePort()];
  const ownerKey = randomBytes(32).toString('hex');
  const stubUrl = `http://127.0.0.1:${stubPort}/v1`;
  // A fake `cursor-agent` first on the daemon's PATH: it speaks ACP and needs the CLI's own tools.
  const fixture = join(targetDir, 'debug', process.platform === 'win32' ? 'native-cli-fixture.exe' : 'native-cli-fixture');
  const fakeBin = join(tmp, 'fakebin');
  mkdirSync(fakeBin, { recursive: true });
  if (existsSync(fixture)) copyFileSync(fixture, join(fakeBin, process.platform === 'win32' ? 'cursor-agent.exe' : 'cursor-agent'));
  const env = { ...process.env, PATH: `${fakeBin}${delimiter}${process.env.PATH ?? process.env.Path ?? ''}`, BOT_DATA_DIR: dataDir, BOT_SECRETS_BACKEND: 'file', BOT_SECRETS_PASSPHRASE: 'smoke-passphrase-smoke-passphrase', BOT_ALLOWED_ORIGINS: `http://127.0.0.1:${uiPort},http://localhost:${uiPort}` };
  // Async on purpose: the stub lives in this process and must keep answering while the CLI tests a provider.
  const cli = (a, extraEnv = {}) => new Promise((ok) => {
    const c = spawn(bot, ['--data-dir', dataDir, '--secrets-backend', 'file', ...a], { env: { ...env, ...extraEnv }, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    c.stdout.on('data', (d) => { stdout += d; });
    c.stderr.on('data', (d) => { stderr += d; });
    c.on('close', (code) => ok({ status: code, stdout, stderr }));
  });
  let ok = false;
  let browser;
  try {
    const stub = await startStub(stubPort);
    // web_search goes to the stub (SearXNG instance, trusted loopback host): no network, and its answer is untrusted content.
    writeFileSync(join(dataDir, 'web.toml'), `[search]
searxng_url = "http://127.0.0.1:${stubPort}/"
order = ["searxng"]
`);
    // The scripted provider: OpenAI-compatible, pointed at the stub.
    const add = await cli(['providers', 'add', 'stub', '--base-url', stubUrl, '--model', 'stub-small', '--key-env', 'SMOKE_STUB_KEY'], { SMOKE_STUB_KEY: 'stub-key' });
    if (add.status !== 0) throw new Error(`providers add failed: ${add.stderr || add.stdout}`);
    await cli(['providers', 'default', 'stub', '--model', 'stub-small']);

    const daemon = spawn(bot, ['--data-dir', dataDir, '--port', String(daemonPort), '--secrets-backend', 'file', 'serve', '--owner-key-stdin'], { env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    children.push(daemon);
    let daemonOut = '';
    daemon.stdout.on('data', (d) => { daemonOut += d; });
    daemon.stderr.on('data', (d) => { daemonOut += d; });
    daemon.stdin.write(`${ownerKey}\n`);
    const base = `http://127.0.0.1:${daemonPort}`;
    await waitFor('daemon health', async () => (await fetch(`${base}/v1/health`)).ok, 90000).catch((e) => { throw new Error(`${e.message}\n${daemonOut.slice(-2000)}`); });
    const token = (await waitFor('api token', () => readFileSync(join(dataDir, 'api-token'), 'utf8').trim())).trim();

    const vite = spawn(process.execPath, [join(root, 'node_modules/vite/bin/vite.js'), '--port', String(uiPort), '--strictPort', '--host', '127.0.0.1'], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true, env: { ...process.env, VITE_MOCK: '' } });
    children.push(vite);
    await waitFor('vite', async () => (await fetch(`http://127.0.0.1:${uiPort}/`)).ok, 60000);

    browser = await chromium.launch({ executablePath: chromePath, headless: !args.includes('--headed'), args: ['--no-sandbox'] });
    const shimmed = new Set();
    const connection = () => ({ base_url: base, token, started_by_app: true, fake_provider: false, data_dir: dataDir });
    const invoke = async (cmd, a) => {
      shimmed.add(cmd);
      switch (cmd) {
        case 'daemon_connect':
        case 'daemon_restart': return connection();
        case 'owner_key': return ownerKey;
        case 'providers_cli': {
          const act = a?.action;
          const argv = ['providers'];
          const extra = {};
          if (act === 'list') argv.push('list');
          else if (act === 'add') {
            argv.push('add', a.id);
            // No network: the well-known providers are pointed at the stub.
            if (['openai', 'openrouter', 'deepseek'].includes(a.id)) argv.push('--base-url', stubUrl);
            else if (a.base_url) argv.push('--base-url', a.base_url);
            if (a.model) argv.push('--model', a.model);
            if (a.api_key) { argv.push('--key-env', 'PRAXIBOT_UI_API_KEY'); extra.PRAXIBOT_UI_API_KEY = a.api_key; }
          } else if (act === 'test') argv.push('test', a.id);
          else if (act === 'default') { argv.push('default', a.id); if (a.model) argv.push('--model', a.model); }
          const r = await cli(argv, extra);
          return { code: r.status ?? -1, stdout: r.stdout ?? '', stderr: r.stderr ?? '' };
        }
        case 'get_autostart':
        case 'autostart_supported':
        case 'set_autostart':
        case 'is_human_driving': return false;
        case 'get_control_state':
        case 'set_stop_hotkey':
        case 'start_bot_run':
        case 'pause_bot_run':
        case 'resume_bot_run':
        case 'stop_bot_run':
        case 'finish_bot_run':
        case 'ack_stop':
        case 'dismiss_stop_widget':
        case 'take_the_wheel':
        case 'give_back_control': return CONTROL;
        case 'plugin:event|listen': return 1;
        case 'plugin:store|load':
        case 'plugin:store|get':
        case 'plugin:store|set': throw new Error('store plugin is not available in the browser run');
        default: return null;
      }
    };
    const ctx = { browser, invoke, uiUrl: `http://127.0.0.1:${uiPort}`, shots, workDir, stubLog, base, token, ownerKey, only: flag('only'), log };
    const results = await flows(ctx);
    ok = results.every((r) => r.ok);
    log(`\nshimmed Tauri commands: ${[...shimmed].sort().join(', ')}`);
    log(`stub saw ${stubLog.length} requests`);
    log(`\n${results.map((r) => `${r.ok ? 'PASS' : 'FAIL'}  ${r.name}${r.ok ? '' : `\n        ${r.error}`}`).join('\n')}`);
    stub.close();
  } finally {
    await browser?.close().catch(() => {});
    for (const c of children) killTree(c);
    if (!args.includes('--keep')) rmSync(tmp, { recursive: true, force: true });
    else log(`kept ${tmp}`);
  }
  return ok;
}

main().then((ok) => process.exit(ok ? 0 : 1), (e) => { log(`smoke: ${e.stack ?? e}`); for (const c of children) killTree(c); process.exit(2); });
