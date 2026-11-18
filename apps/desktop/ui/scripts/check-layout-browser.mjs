// Bounded headless-Chrome layout run.  Local and optional, not part of CI.
//   node scripts/check-layout-browser.mjs [--require] [--only=<n,n|substring>] [--budget=<seconds>]
// Builds the mock bundle, serves it with `vite preview`, opens each case in a fresh headless Chrome
// with `--dump-dom` and reads the JSON the dev-only layout probe appended.  Everything it creates
// lives in one temp dir that is deleted in `finally`; Chrome and vite are killed as process trees.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath, URL, URLSearchParams } from 'node:url';
import { clearTimeout, setTimeout } from 'node:timers';
import { setTimeout as delay } from 'node:timers/promises';
import { validateProbe } from '../src/lib/layoutProbeCheck.ts';

const args = process.argv.slice(2);
const flag = (name) => args.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const mustRun = args.includes('--require');
const only = flag('only');
const budgetMs = Number(flag('budget') ?? 480) * 1000;
const CASE_TIMEOUT = 20000;
const root = fileURLToPath(new URL('../', import.meta.url));
const CHROMES = [process.env.CHROME_BIN, 'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium']
  .filter(Boolean);
const chrome = CHROMES.find((p) => existsSync(p));
if (!chrome) {
  process.stdout.write('SKIPPED: no Chrome found (set CHROME_BIN)\n');
  process.exit(mustRun ? 1 : 0);
}

// 24 cases: widths x style x theme (18), plus 6 specials.  `rt=ignore`: measure the requested style
// even when the host OS asks for reduced transparency (headless Chrome reports that setting too).
const cases = [];
for (const width of [760, 1400, 1920]) {
  for (const style of ['solid', 'glass', 'line']) {
    for (const theme of ['clay', 'obsidian']) {
      cases.push({ name: `${width}/${style}/${theme}/open`, width, query: `theme=${theme}&style=${style}&panel=1&rail=264` });
    }
  }
}
cases.push(
  { name: '1400/glass/clay/closed', width: 1400, query: 'theme=clay&style=glass&rail=264' },
  { name: '1400/glass/clay/open/stored1600', width: 1400, query: 'theme=clay&style=glass&panel=1&rail=264&stored=1600' },
  { name: '1400/glass/clay/open/corners=round', width: 1400, query: 'theme=clay&style=glass&panel=1&rail=264&corners=round' },
  { name: '1400/solid/clay/open/backdrop', width: 1400, query: 'theme=clay&style=solid&panel=1&rail=264&backdrop=aurora' },
  { name: '1400/glass/obsidian/open/backdrop', width: 1400, query: 'theme=obsidian&style=glass&panel=1&rail=264&backdrop=aurora' },
  { name: '900/glass/clay/open/rail440', width: 900, query: 'theme=clay&style=glass&panel=1&rail=440' }
);
const searchMode = args.includes('--search');
const contentMode = args.includes('--content');
const probeId = searchMode ? 'search-probe' : contentMode ? 'content-probe' : 'layout-probe';
if (searchMode) {
  cases.length = 0;
  cases.push({ name: '760/glass/clay/open/search', width: 760, probe: 'search', query: 'theme=clay&style=glass&panel=1&rail=264' });
}
if (contentMode) {
  // The narrowest fitted panel (stored 260) holding the live-view block, in both locales.
  cases.length = 0;
  const locale = (l) => JSON.parse(readFileSync(resolve(root, `src/i18n/locales/${l}.json`), 'utf8')).liveView;
  for (const lang of ['tr', 'en']) {
    for (const style of ['glass', 'solid']) {
      cases.push({ name: `760/${style}/clay/panel260/${lang}/live-view-paused`, width: 760, probe: 'content',
        query: `theme=clay&style=${style}&panel=1&rail=264&stored=260&lvState=paused&lang=${lang}&reacq=${encodeURIComponent(locale(lang).reacquireAction)}` });
    }
  }
}
const selected = !only ? cases
  : /^[\d,]+$/.test(only) ? only.split(',').map((i) => cases[Number(i)]).filter(Boolean)
  : cases.filter((c) => c.name.includes(only));

/** What each case asks for, derived from its query (validated against what the probe measured). */
const expectOf = (c) => {
  const q = new URLSearchParams(c.query);
  return {
    open: q.get('panel') === '1',
    theme: q.get('theme') ?? undefined,
    style: q.get('style') ?? undefined,
    corners: q.get('corners') ?? undefined,
    backdrop: q.get('backdrop') ? 'on' : 'off'
  };
};

const killTree = (pid) => {
  if (!pid) return;
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  else { try { process.kill(pid, 'SIGKILL'); } catch { /* already gone */ } }
};
const freePort = () => new Promise((ok, fail) => {
  const s = createServer(); s.unref(); s.on('error', fail);
  s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => ok(port)); });
});
const run = (cmd, argv, opts) => new Promise((ok) => {
  const child = spawn(cmd, argv, { windowsHide: true, stdio: opts.stdio });
  let out = '';
  child.stdout?.on('data', (d) => { out += d; });
  const timer = setTimeout(() => { killTree(child.pid); }, opts.timeout);
  child.on('exit', () => { clearTimeout(timer); killTree(child.pid); ok(out); });
});

const tmp = mkdtempSync(join(tmpdir(), 'praxi-layout-'));
const started = Date.now();
let preview;
let failed = 0;
try {
  const vite = resolve(root, 'node_modules/vite/bin/vite.js');
  const dist = join(tmp, 'dist');
  const build = spawnSync(process.execPath, [vite, 'build', '--outDir', dist, '--emptyOutDir'],
    { cwd: root, env: { ...process.env, VITE_MOCK: '1' }, encoding: 'utf8', windowsHide: true, timeout: 120000 });
  if (build.status !== 0) throw new Error(`vite build failed:\n${build.stdout}\n${build.stderr}`);
  const port = await freePort();
  preview = spawn(process.execPath, [vite, 'preview', '--outDir', dist, '--port', String(port), '--strictPort', '--host', '127.0.0.1'],
    { cwd: root, windowsHide: true, stdio: 'ignore' });
  for (let n = 0; n < 50; n++) {
    try { if ((await globalThis.fetch(`http://127.0.0.1:${port}/`)).ok) break; } catch { /* not up yet */ }
    await delay(200);
  }
  let narrowest = Infinity;
  for (const [i, c] of selected.entries()) {
    if (Date.now() - started > budgetMs) { process.stdout.write(`BUDGET exhausted after ${i} cases\n`); failed++; break; }
    let out = '';
    for (let attempt = 0; attempt < 4 && !out.includes(probeId); attempt++) {
      out = await run(chrome, ['--headless=new', '--disable-gpu', '--no-sandbox', '--hide-scrollbars',
      '--force-device-scale-factor=1', `--window-size=${c.width},900`, `--virtual-time-budget=${process.env.VTB ?? 30000}`,
      `--user-data-dir=${join(tmp, `profile-${i}-${attempt}`)}`, '--dump-dom', `http://127.0.0.1:${port}/?mock=1&probe=${c.probe ?? 'layout'}&rt=ignore&${c.query}`],
      { stdio: ['ignore', 'pipe', args.includes('--debug') ? 'inherit' : 'ignore'], timeout: CASE_TIMEOUT });
    }
    if (searchMode || contentMode) {
      const sm = new RegExp(`<script id="${probeId}" type="application/json">([\\s\\S]*?)</script>`).exec(out);
      if (!sm) { process.stdout.write(`FAIL ${c.name}: no ${probeId} output
`); failed++; continue; }
      const data = JSON.parse(sm[1].replace(/&quot;/g, '"').replace(/&amp;/g, '&'));
      if (contentMode) {
        const bad = [];
        if (data.error) bad.push(data.error);
        else {
          const items = [['status', data.status], ...data.buttons.map((b) => [`button "${b.text}"`, b])];
          for (const [label, r] of items) if (r.right > data.contentRight + 0.5 || r.left < 0) bad.push(`${label} right ${r.right} > panel content right ${data.contentRight}`);
          if (data.row.scrollWidth > data.row.clientWidth + 0.5) bad.push(`recovery row scrolls (${data.row.scrollWidth} > ${data.row.clientWidth})`);
          if (data.block.scrollWidth > data.block.clientWidth + 0.5) bad.push(`live-view block overflows (${data.block.scrollWidth} > ${data.block.clientWidth})`);
        }
        process.stdout.write(`${bad.length ? 'FAIL' : 'ok  '} ${c.name} panel=${data.panelWidth} contentRight=${data.contentRight} status=${data.status ? `${data.status.left.toFixed(0)}..${data.status.right.toFixed(0)} "${data.statusText}"` : '-'} buttons=${(data.buttons ?? []).map((b) => `"${b.text}" ${b.left.toFixed(0)}..${b.right.toFixed(0)} y${b.top.toFixed(0)}`).join(' | ')}
`);
        for (const line of bad) process.stdout.write(`       - ${line}
`);
        if (bad.length) failed++;
      } else {
        process.stdout.write(`${c.name}: ${JSON.stringify(data)}
`);
      }
      continue;
    }
    const m = /<script id="layout-probe" type="application\/json">([\s\S]*?)<\/script>/.exec(out);
    if (!m) { process.stdout.write(`FAIL ${c.name}: no probe output (timeout or crash)
`); failed++; continue; }
    const p = JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&amp;/g, '&'));
    const expect = expectOf(c);
    const v = validateProbe(p, expect);
    const open = expect.open;
    const clip = open && p.panelInner ? p.panelInner.right - p.panelContentRight : 0;
    narrowest = Math.min(narrowest, p.main ? p.main.width : Infinity);
    const f = (b) => (b ? `${b.left.toFixed(1)}..${b.right.toFixed(1)}(w${b.width.toFixed(1)})` : '-');
    process.stdout.write(`${v.length ? 'FAIL' : 'ok  '} ${c.name} vp=${p.viewport?.width} float=${p.float} rail=${f(p.rail)} main=${f(p.main)} panel=${f(p.panel)} borders=${JSON.stringify(p.panelBorders)} inner=${f(p.panelInner)} contentRight=${p.panelContentRight} clip=${clip.toFixed(1)} doc=${p.document?.scrollWidth}/${p.document?.clientWidth}${p.error ? ` error=${p.error}` : ''}
`);
    for (const line of v) process.stdout.write(`       - ${line}\n`);
    if (v.length) failed++;
  }
  process.stdout.write(`${selected.length} cases, ${failed} failing, narrowest main ${narrowest.toFixed(1)}px, ${((Date.now() - started) / 1000).toFixed(0)}s\n`);
} catch (e) {
  failed++;
  process.stdout.write(`ERROR ${e.message}\n`);
} finally {
  killTree(preview?.pid);
  await delay(300);
  try { rmSync(tmp, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 }); } catch { /* best effort */ }
}
process.exit(failed ? 1 : 0);
