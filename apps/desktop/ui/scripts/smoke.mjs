// Real-app smoke run: the production UI bundle against a real local daemon
// (`bot serve`, debug build) that talks to a tiny scripted OpenAI-compatible stub.
//   node scripts/smoke.mjs [--no-build] [--only=<substring>] [--keep] [--headed]
// Everything it starts is stopped in `finally`; it attempts to remove its temp dir unless --keep.
//
// Tauri commands shimmed for the browser run (everything else answers `null` and is listed at the end):
//   daemon_connect / daemon_restart  -> the daemon this script started (started_by_app: true)
//   owner_key                        -> the owner key handed to `bot serve --owner-key-stdin`
//   providers_cli                    -> runs the real `bot providers ...` (API base URLs of the
//                                       well-known providers are pointed at the stub; no network)
//   get_autostart / autostart_supported / set_autostart, is_human_driving
//   get_control_state and the other stop/pause/takeover commands -> an idle control snapshot
//   plugin:store|*                   -> rejected, so settings fall back to localStorage (as in a browser)
/* global AbortController, AbortSignal, fetch, URL, setTimeout, clearTimeout */
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { appendFileSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import http from 'node:http';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, resolve } from 'node:path';
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
const logs = join(shots, 'logs');
const targetDir = resolve(repo, process.env.CARGO_TARGET_DIR ?? 'target');
const windows = process.platform === 'win32';
const executable = (name) => `${name}${windows ? '.exe' : ''}`;
const bot = join(targetDir, 'debug', executable('bot'));
const fixture = join(targetDir, 'debug', executable('native-cli-fixture'));
const CHROMES = ['C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium'];
const chromePath = process.env.CHROME_BIN ?? CHROMES.find((p) => existsSync(p));

const children = new Set();
const log = (m) => {
  appendFileSync(join(logs, 'smoke.log'), `${m}\n`);
  process.stdout.write(`${m}\n`);
};
const freePort = () => new Promise((ok, no) => {
  const s = createServer();
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
  s.on('error', no);
});
const killTree = (c) => {
  if (!c.pid) return;
  if (windows) {
    const killed = spawnSync('taskkill', ['/PID', String(c.pid), '/T', '/F'], { stdio: 'ignore', timeout: 5000, windowsHide: true });
    // The daemon restricts the rights taskkill can request when opening its PID.
    // The harness already owns the child process handle from spawn.
    if (killed.status !== 0) c.kill();
  }
  else try { process.kill(-c.pid); } catch { /* gone */ }
};
function start(program, argv, options) {
  const child = spawn(program, argv, { detached: !windows, windowsHide: true, ...options });
  children.add(child);
  child.once('close', () => children.delete(child));
  child.on('error', (error) => log(`${program}: ${error.message}`));
  return child;
}

function capture(child, name) {
  const path = join(logs, `${name}.log`);
  writeFileSync(path, '');
  for (const stream of [child.stdout, child.stderr]) stream.on('data', (chunk) => appendFileSync(path, chunk));
}

function isolatedEnv(tmp, fakeBin) {
  // Keep the OS settings needed by the harness. Real provider keys, proxies,
  // CLI overrides and user config directories stay out of the child processes.
  const env = {};
  for (const name of ['SystemRoot', 'WINDIR', 'ComSpec', 'PATHEXT', 'DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS', 'XDG_SESSION_TYPE']) {
    if (process.env[name]) env[name] = process.env[name];
  }
  const systemPath = windows
    ? [join(env.SystemRoot, 'System32'), join(env.SystemRoot, 'System32/WindowsPowerShell/v1.0')]
    : ['/usr/bin', '/bin'];
  const home = join(tmp, 'home');
  const appData = join(home, 'AppData/Roaming');
  const localAppData = join(home, 'AppData/Local');
  for (const dir of [appData, localAppData]) mkdirSync(dir, { recursive: true });
  return {
    ...env,
    PATH: [fakeBin, dirname(process.execPath), ...systemPath].join(delimiter),
    HOME: home,
    USERPROFILE: home,
    APPDATA: appData,
    LOCALAPPDATA: localAppData,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_DATA_HOME: join(home, '.local/share'),
    XDG_CACHE_HOME: join(home, '.cache'),
    TMP: tmp,
    TEMP: tmp,
    TMPDIR: tmp
  };
}

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
function scripted(messages, tools = []) {
  // The loop adds a user-role action ledger after a tool call. The fixture
  // still follows the original user turn and replies with the tool result.
  const contentText = (content) => typeof content === 'string' ? content
    : (content ?? []).map((part) => part.text ?? '').join('');
  const userIndex = messages.findLastIndex((m) => m.role === 'user'
    && !contentText(m.content).startsWith('[action ledger, not written by the user:'));
  const text = contentText(messages[userIndex]?.content);
  const lines = text.split('\n').map((l) => l.trim()).filter(Boolean);
  const calls = lines.length > 0 && lines.every((l) => l.startsWith('call:'))
    ? lines.map((l) => { const r = l.slice(5); const i = r.indexOf(' '); return i < 0 ? [r, '{}'] : [r.slice(0, i), r.slice(i + 1)]; }) : null;
  // `slow:<ms>`: the model starts answering and then stalls (Stop cases hold the provider request open).
  const slow = /^slow:(\d+)/.exec(text.trim());
  if (slow) return { slow: Number(slow[1]) };
  const after = messages.slice(userIndex + 1);
  const toolReplies = after.filter((m) => m.role === 'tool');
  // Deferred tool surface: a tool that is not offered yet is loaded with
  // tool_describe first, like a real model would, then called.
  const callNames = new Map(after.filter((m) => m.role === 'assistant')
    .flatMap((m) => m.tool_calls ?? []).map((c) => [c.id, c.function?.name]));
  const isDescribe = (m) => callNames.get(m.tool_call_id) === 'tool_describe';
  const done = toolReplies.filter((m) => !isDescribe(m)).length;
  const offered = new Set(tools.map((t) => t.function?.name));
  if (calls && done < calls.length) {
    const [name, args] = calls[done];
    const lastReply = toolReplies.at(-1);
    if (!offered.has(name) && offered.has('tool_describe') && !(lastReply && isDescribe(lastReply))) {
      return { call: { name: 'tool_describe', args: JSON.stringify({ name }), id: `call_d${toolReplies.length + 1}` } };
    }
    return { call: { name, args, id: `call_${done + 1}` } };
  }
  const last = toolReplies.at(-1);
  if (last) return { text: `tool said: ${String(last.content).slice(0, 200)}` };
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
        const out = scripted(payload.messages ?? [], payload.tools ?? []);
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
async function runSmoke(tmp, runtime) {
  const dataDir = join(tmp, 'data');
  const workDir = join(tmp, 'home', 'work');
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(workDir, { recursive: true });
  writeFileSync(join(workDir, 'hello.txt'), 'hello from the smoke run\n');
  const stub = await startStub(0);
  runtime.stub = stub;
  const { port: stubPort } = stub.address();
  const daemonPort = await freePort();
  let uiPort = await freePort();
  while (uiPort === daemonPort) uiPort = await freePort();
  const ownerKey = randomBytes(32).toString('hex');
  const stubUrl = `http://127.0.0.1:${stubPort}/v1`;
  // A fake `cursor-agent` first on the daemon's PATH: it speaks ACP and needs the CLI's own tools.
  const fakeBin = join(tmp, 'fakebin');
  mkdirSync(fakeBin, { recursive: true });
  copyFileSync(fixture, join(fakeBin, executable('cursor-agent')));
  const isolated = isolatedEnv(tmp, fakeBin);
  const env = {
    ...isolated,
    BOT_DATA_DIR: dataDir,
    BOT_SECRETS_BACKEND: 'file',
    BOT_SECRETS_PASSPHRASE: 'smoke-passphrase-smoke-passphrase',
    BOT_BROWSER_PATH: chromePath,
    BOT_ALLOWED_ORIGINS: `http://127.0.0.1:${uiPort},http://localhost:${uiPort}`
  };
  // Async on purpose: the stub lives in this process and must keep answering while the CLI tests a provider.
  const cli = (a, extraEnv = {}) => new Promise((ok) => {
    const c = start(bot, ['--data-dir', dataDir, '--secrets-backend', 'file', ...a], { cwd: workDir, env: { ...env, ...extraEnv }, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    c.stdout.on('data', (d) => { stdout += d; });
    c.stderr.on('data', (d) => { stderr += d; });
    c.on('close', (code) => ok({ status: code, stdout, stderr }));
  });
  // web_search goes to the stub (SearXNG instance, trusted loopback host): no network, and its answer is untrusted content.
  writeFileSync(join(dataDir, 'web.toml'), `[search]\nsearxng_url = "http://127.0.0.1:${stubPort}/"\norder = ["searxng"]\n`);
  // The scripted provider: OpenAI-compatible, pointed at the stub.
  const add = await cli(['providers', 'add', 'stub', '--base-url', stubUrl, '--model', 'stub-small', '--key-env', 'SMOKE_STUB_KEY'], { SMOKE_STUB_KEY: 'stub-key' });
  if (add.status !== 0) throw new Error(`providers add failed: ${add.stderr || add.stdout}`);
  const selected = await cli(['providers', 'default', 'stub', '--model', 'stub-small']);
  if (selected.status !== 0) throw new Error(`providers default failed: ${selected.stderr || selected.stdout}`);
  const tested = await cli(['providers', 'test', 'stub']);
  if (tested.status !== 0) throw new Error(`loopback provider unreachable: ${tested.stderr || tested.stdout}`);

  const daemon = start(bot, ['--data-dir', dataDir, '--port', String(daemonPort), '--secrets-backend', 'file', 'serve', '--owner-key-stdin'], { cwd: workDir, env, stdio: ['pipe', 'pipe', 'pipe'] });
  capture(daemon, 'daemon');
  daemon.stdin.on('error', (error) => log(`daemon stdin: ${error.message}`));
  daemon.stdin.end(`${ownerKey}\n`);
  const base = `http://127.0.0.1:${daemonPort}`;
  await waitFor('daemon health (see daemon.log)', async () => (await fetch(`${base}/v1/health`)).ok, 90000);
  const token = (await waitFor('api token', () => readFileSync(join(dataDir, 'api-token'), 'utf8').trim())).trim();
  // Health alone succeeds on a displayless daemon, but UI-created bots target
  // this PC and never reach the provider without its local desktop backend.
  const doctor = await fetch(`${base}/v1/doctor`, {
    headers: { authorization: `Bearer ${token}`, 'x-praxi-owner-key': ownerKey },
    signal: AbortSignal.timeout(15000)
  });
  if (!doctor.ok) throw new Error(`desktop preflight returned HTTP ${doctor.status} (see daemon.log)`);
  const desktop = (await doctor.json()).checks?.find((check) => check.id === 'desktop_automation');
  if (desktop?.status !== 'ok') {
    throw new Error(`local desktop unavailable: ${desktop?.detail ?? 'no backend'}; on Linux run under xvfb-run and dbus-run-session (see daemon.log)`);
  }
  log(`preflight: loopback provider reachable; desktop ${desktop.detail}`);

  // Serve the production bundle so cold Vite dependency discovery cannot reload
  // the page in the middle of the first-run flow.
  const dist = join(tmp, 'dist');
  const viteBin = join(root, 'node_modules/vite/bin/vite.js');
  const viteOptions = { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: { ...isolated, VITE_MOCK: '' } };
  const build = start(process.execPath, [viteBin, 'build', '--outDir', dist], viteOptions);
  capture(build, 'vite-build');
  if (await new Promise((done) => build.once('close', done)) !== 0) throw new Error('UI build failed (see vite-build.log)');
  const uiUrl = `http://127.0.0.1:${uiPort}`;
  const vite = start(process.execPath, [viteBin, 'preview', '--outDir', dist, '--port', String(uiPort), '--strictPort', '--host', '127.0.0.1'], viteOptions);
  capture(vite, 'vite');
  await waitFor('vite (see vite.log)', async () => (await fetch(uiUrl)).ok);

  const browser = await chromium.launch({ executablePath: chromePath, headless: !args.includes('--headed'), env: isolated });
  runtime.browser = browser;
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
          // Model setup in this run always uses the loopback fixture.
          if (!['openai', 'openrouter', 'deepseek'].includes(a.id)) throw new Error(`provider ${a.id} has no smoke fixture`);
          argv.push('--base-url', stubUrl);
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
  const ctx = { browser, invoke, uiUrl, shots, workDir, stubLog, base, token, ownerKey, only: flag('only'), log };
  const results = await flows(ctx);
  log(`\nshimmed Tauri commands: ${[...shimmed].sort().join(', ')}`);
  log(`stub saw ${stubLog.length} requests`);
  log(`\n${results.map((r) => `${r.ok ? 'PASS' : 'FAIL'}  ${r.name}${r.ok ? '' : `\n        ${r.error}`}`).join('\n')}`);
  if (results.length === 0) throw new Error('no smoke flows selected');
  return results.every((r) => r.ok);
}

async function main() {
  mkdirSync(logs, { recursive: true });
  writeFileSync(join(logs, 'smoke.log'), '');
  if (!chromePath || !existsSync(chromePath)) throw new Error('no Chrome found (set CHROME_BIN)');
  if (!args.includes('--no-build')) {
    log('building the debug daemon and CLI fixture...');
    const build = spawnSync('cargo', ['build', '--locked', '-p', 'cli', '-p', 'cli-provider', '--bin', 'bot', '--bin', 'native-cli-fixture'], { cwd: repo, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: targetDir, CARGO_INCREMENTAL: '0', CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '3' } });
    if (build.status !== 0) throw new Error('cargo build failed');
  }
  for (const binary of [bot, fixture]) {
    if (!existsSync(binary)) throw new Error(`missing ${binary}`);
  }

  const tmp = mkdtempSync(join(tmpdir(), 'praxi-smoke-'));
  const runtime = {};
  const timer = new AbortController();
  const deadline = delay(8 * 60_000, undefined, { signal: timer.signal }).then(async () => {
    log('smoke exceeded its 8-minute runtime limit');
    const pages = runtime.browser?.contexts().flatMap((context) => context.pages()) ?? [];
    await Promise.allSettled(pages.map((page, i) => page.screenshot({ path: join(shots, `timeout-${i + 1}.png`), timeout: 5000 })));
    throw new Error('smoke runtime timed out');
  });
  let ok;
  let allStopped;
  try {
    ok = await Promise.race([runSmoke(tmp, runtime), deadline]);
  } finally {
    timer.abort();
    runtime.stub?.close();
    runtime.stub?.closeAllConnections();
    await runtime.browser?.close().catch(() => {});
    const stopped = [...children].map((child) => {
      const closed = new Promise((done) => child.once('close', done));
      killTree(child);
      return closed;
    });
    allStopped = await Promise.race([Promise.all(stopped).then(() => true), delay(5000, false, { ref: false })]);
    if (args.includes('--keep')) log(`kept ${tmp}`);
    else {
      // Windows can refuse removal of the daemon's ACL-protected token file.
      // Keep the isolated directory in that case and preserve its ACL.
      const removal = rm(tmp, { recursive: true, force: true }).then(() => true, () => false);
      if (!await Promise.race([removal, delay(5000, false, { ref: false })])) log(`kept ${tmp}: temporary files could not be removed`);
    }
  }
  if (ok && !allStopped) throw new Error('a smoke child did not exit');
  return ok;
}

main().then((ok) => process.exit(ok ? 0 : 1), (e) => { log(`smoke: ${e.stack ?? e}`); for (const c of children) killTree(c); process.exit(2); });
