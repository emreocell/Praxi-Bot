// The flows of the real-app smoke run. Driven by scripts/smoke.mjs; no test framework:
// each step is a function, a thrown error fails it, and every step ends with a screenshot plus a
// check for error banners, danger toasts, console errors and failed requests.
/* global window, document, location, fetch, URL, console, setInterval, clearInterval, setTimeout */
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import process from 'node:process';
import { join } from 'node:path';

const SETTINGS = JSON.stringify({ onboardingCompleted: true, onboardingSkipped: false, language: 'en' });
const EN = JSON.stringify({ language: 'en' });

/** A browser context whose page believes it runs inside the Tauri app (see smoke.mjs for the shim). */
async function newSession(ctx, { onboarded }) {
  const context = await ctx.browser.newContext({ viewport: { width: 1280, height: 800 }, serviceWorkers: 'block' });
  await context.route('**/*', (route) => {
    const origin = new URL(route.request().url()).origin;
    return origin === ctx.uiUrl || origin === ctx.base ? route.continue() : route.abort();
  });
  await context.exposeFunction('__smokeInvoke', (cmd, args) => ctx.invoke(cmd, args));
  await context.addInitScript(({ settings, fresh }) => {
    try { if (!window.localStorage.getItem('desktop_ui_settings')) window.localStorage.setItem('desktop_ui_settings', settings); } catch { /* storage unavailable */ }
    void fresh;
    window.__errs = [];
    window.addEventListener("error", (e) => window.__errs.push(`error: ${e.message} ${e.error && e.error.stack}`));
    window.addEventListener("unhandledrejection", (e) => window.__errs.push(`rejection: ${e.reason && (e.reason.stack || e.reason)}`));
    const origReport = window.reportError && window.reportError.bind(window);
    window.reportError = (e) => { window.__errs.push(`reportError: ${e && e.stack}`); if (origReport) origReport(e); };
    const ce = console.error.bind(console);
    console.error = (...a) => { window.__errs.push(a.map((x) => (x && x.stack) || String(x)).join(' ').slice(0, 1500)); ce(...a); };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    let cb = 1;
    window.__TAURI_INTERNALS__ = {
      invoke: (cmd, args) => window.__smokeInvoke(cmd, args ?? {}),
      transformCallback: () => cb++,
      unregisterCallback: () => {},
      convertFileSrc: (p) => p,
      metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } }
    };
  }, { settings: onboarded ? SETTINGS : EN, fresh: !onboarded });
  const page = await context.newPage();
  const problems = [];
  const failed = [];
  const logoUrl = `${ctx.base}/v1/composio/logos/`;
  if (process.env.SMOKE_DEBUG) page.on('console', (m) => ctx.log(`[console.${m.type()}] ${m.text().slice(0, 1500)} @ ${m.location().url}:${m.location().lineNumber}`));
  if (process.env.SMOKE_DEBUG) page.on('requestfailed', (r) => ctx.log(`[requestfailed] ${r.url()} ${r.failure()?.errorText}`));
  page.on('console', (m) => {
    const url = m.location().url;
    if (m.type() !== 'error' || url.endsWith('/favicon.ico')) return;
    // The offline Apps catalog can lack logo artwork. The tile supplies a fallback.
    if (url.startsWith(logoUrl) && m.text().includes('status of 404')) return;
    problems.push(`console: ${m.text().slice(0, 300)}`);
  });
  page.on('pageerror', (e) => problems.push(`pageerror: ${String(e).slice(0, 300)}`));
  page.on('response', (r) => {
    if (r.status() === 404 && r.url().startsWith(logoUrl)) return;
    if (r.status() >= 400 && r.url().includes('/v1/')) failed.push(`${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`);
  });
  page.on('response', (r) => { if (r.status() >= 500 && r.url().includes('/v1/')) problems.push(`HTTP ${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`); });
  return { context, page, problems, failed };
}

/** What a person would call "an error on screen" right now. */
async function visibleErrors(page) {
  return page.evaluate(() => {
    const out = [];
    for (const el of document.querySelectorAll('[role="alert"],[data-testid="error-notice"],[data-testid="toast"][class*="danger"],[data-testid="fr-error"],[data-testid="daemon-offline-empty-state"],[data-testid="fr-service-down"]')) {
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0 && el.textContent?.trim()) out.push(el.textContent.trim().slice(0, 240));
    }
    return out;
  });
}

export async function flows(ctx) {
  const results = [];
  let n = 0;
  const run = async (session, name, fn, opts = {}) => {
    if (ctx.only && !ctx.only.split(',').some((part) => name.includes(part))) return;
    n += 1;
    const slug = `${String(n).padStart(2, '0')}-${name.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}`;
    const { page, problems, failed } = session;
    ctx.log(`RUN  ${name}`);
    problems.length = 0;
    failed.length = 0;
    let error = null;
    try {
      await fn(page);
      await page.waitForTimeout(400);
      const errs = await visibleErrors(page);
      if (errs.length && !opts.expectError) throw new Error(`error on screen: ${errs.join(' | ')}`);
      // A step that expects the daemon to refuse something sees a 4xx in the console: that is the point of it.
      let real = opts.expectError ? problems.filter((p) => !p.startsWith('console: Failed to load resource')) : problems;
      // A daemon starved for a while (a loaded machine) cannot renew view-session heartbeats and expires the
      // session: the page's late renewals then get 404 and the browser logs each one. That is the daemon
      // doing its job, so a step whose only failed requests are 404s on /v1/view/sessions/ is not broken by it.
      if (failed.length > 0 && failed.every((f) => f.startsWith('404 ') && f.includes(' /v1/view/sessions/'))) real = real.filter((p) => !p.startsWith('console: Failed to load resource'));
      if (real.length) throw new Error(`browser problems: ${[...new Set(real)].join(' | ')} (failed requests: ${[...new Set(failed)].join(', ') || 'none'})`);
    } catch (e) {
      error = String(e.message ?? e);
    }
    if (error && process.env.SMOKE_DEBUG) {
      const dump = await page.evaluate(() => `${location.href} root=${document.getElementById("root")?.innerHTML.length} ${(window.__errs ?? []).join(" --- ")}`).catch((e) => String(e));
      ctx.log(`[debug] ${dump.slice(0, 4000)}`);
    }
    await page.screenshot({ path: join(ctx.shots, `${slug}.png`), timeout: 5000 })
      .catch((e) => ctx.log(`could not capture ${slug}.png: ${e.message}`));
    results.push({ name, ok: !error, error });
    ctx.log(`${error ? 'FAIL' : 'ok  '} ${name}${error ? `  - ${error}` : ''}`);
    // Later flows share the bots/chats created above. Preserve the first
    // screenshot and fail promptly instead of cascading into minutes of waits.
    if (error) throw new Error(`${name}: ${error}`);
  };
  // Generous: the machine this runs on is often busy with builds, and a flow that is merely slow must not look broken.
  const T = { timeout: 30000 };
  // A run that executes a shell command can take 25 s on a loaded machine.
  const SLOW = { timeout: 90000 };

  // ---- first run: skip path, then the API-key path against the stub
  const first = await newSession(ctx, { onboarded: false });
  await run(first, 'first run: wizard opens once (no loop) and Skip leaves a usable app', async (page) => {
    await page.goto(ctx.uiUrl);
    await page.getByTestId('onboarding-view').waitFor(T);
    await page.getByTestId('onboarding-skip').click();
    await page.getByTestId('finish-setup-hint').waitFor(T);
    await page.reload();
    await page.getByTestId('finish-setup-hint').waitFor(T);
    if (await page.getByTestId('onboarding-view').isVisible()) throw new Error('wizard came back after Skip and reload');
  });
  await first.context.close();

  const wiz = await newSession(ctx, { onboarded: false });
  await run(wiz, 'first run: API-key path connects, picks a model and creates the bot', async (page) => {
    await page.goto(ctx.uiUrl);
    await page.getByTestId('onboarding-view').waitFor(T);
    await page.getByTestId('fr-api-openai').click();
    await page.getByTestId('fr-key').fill('sk-smoke-test-key-123456');
    await page.getByTestId('fr-connect').click();
    // Connecting runs real CLI processes (add + test the provider): slow on a busy machine.
    await page.getByTestId('fr-connected').waitFor({ timeout: 60000 });
    if (!(await page.getByTestId('fr-model').textContent())?.trim()) throw new Error('no model chosen after connecting');
    await page.getByTestId('fr-next').click();
    // Ask keeps the file write approval covered below. Balanced allows workspace edits.
    await page.getByTestId('fr-level-ask').click();
    await page.getByTestId('fr-next').click();
    await page.getByTestId('new-bot-name').fill('Atlas');
    await page.getByTestId('fr-next').click();
    await page.getByTestId('bot-empty-state').waitFor(T);
  });
  await wiz.context.close();

  // ---- the day-to-day flows (wizard done)
  const s = await newSession(ctx, { onboarded: true });
  const composer = (page) => page.getByRole('combobox', { name: /message/i });
  const send = async (page, text) => {
    const c = composer(page);
    await c.fill(text);
    await c.press('Enter');
  };
  const openFirstChat = async (page) => {
    await page.getByTestId('bot-empty-new-chat').or(composer(page)).first().waitFor(T);
    if (await page.getByTestId('bot-empty-new-chat').isVisible()) await page.getByTestId('bot-empty-new-chat').click();
    await composer(page).waitFor(T);
  };

  const gotoChat = async (page, name) => {
    if (!(await composer(page).isVisible().catch(() => false))) {
      await page.locator('[class*="railItem"]', { hasText: name }).first().click();
      await openFirstChat(page);
    }
  };

  await run(s, 'app loads with bots and no wizard', async (page) => {
    await page.goto(ctx.uiUrl);
    await page.getByTestId('home-empty-state').or(page.getByTestId('bot-empty-state')).or(page.getByTestId('chat-timeline')).waitFor(T);
    if (await page.getByTestId('onboarding-view').isVisible()) throw new Error('wizard shown although setup is done');
  });
  await run(s, 'status chip menu opens', async (page) => {
    await page.getByTestId('computer-picker').click();
    await page.getByTestId('home-status').waitFor(T);
    await page.keyboard.press('Escape');
  });
  await run(s, 'create a second bot from the New-bot modal', async (page) => {
    await page.getByRole('button', { name: /new bot/i }).first().click();
    await page.getByTestId('new-bot-name').fill('Nova');
    await page.getByTestId('autonomy-ask').click();
    await page.getByTestId('new-bot-submit').click();
    await page.getByTestId('bot-empty-state').waitFor(T);
  });
  // Created here, early: the New-bot modal's Create stays disabled in the later part of a full run (Auto brain unresolved).
  await run(s, 'a Full-level bot is created', async (page) => {
    await page.getByRole('button', { name: /new bot/i }).first().click();
    await page.getByTestId('new-bot-name').fill('Fulla');
    await page.getByTestId('autonomy-auto').click();
    await page.getByTestId('new-bot-submit').click();
    await page.getByTestId('bot-empty-state').waitFor(T);
    const bots = await (await fetch(`${ctx.base}/v1/bots`, { headers: { authorization: `Bearer ${ctx.token}` } })).json();
    const fulla = bots.find((x) => x.name === 'Fulla');
    if (!fulla || !JSON.stringify(fulla.config).includes('full')) throw new Error('Fulla was not stored at the Full level');
  });
  await run(s, 'open a chat and send a message; the scripted reply arrives', async (page) => {
    await page.locator('[class*="railItem"]', { hasText: 'Atlas' }).first().click();
    await openFirstChat(page);
    await send(page, 'hello smoke');
    await page.getByText(/echo: hello smoke/).first().waitFor(T);
  });
  await run(s, 'model picker: pick another model, it is still chosen after reload', async (page) => {
    await page.getByTestId('model-picker').click();
    const option = page.getByRole('option', { name: /large/i }).first();
    await option.waitFor(T);
    await option.click();
    await page.waitForTimeout(800);
    await page.reload();
    await gotoChat(page, 'Atlas');
    const text = (await page.getByTestId('model-picker').textContent()) ?? '';
    if (!/large/i.test(text)) throw new Error(`model not saved, picker shows "${text.trim()}"`);
  });
  await run(s, 'model picker: a coding-CLI group is never shown with 0 models', async (page) => {
    await page.getByTestId('model-picker').click();
    await page.getByRole('dialog').waitFor(T);
    const counts = await page.locator('[data-testid^="model-picker-group-cli:"] [class*="groupCount"]').allTextContents();
    if (counts.some((c) => c.trim() === '0')) throw new Error(`a CLI group is rendered with 0 models: ${counts.join(',')}`);
    await page.keyboard.press('Escape');
  });
  await run(s, 'a pure-read tool call runs and its result is shown', async (page) => {
    await gotoChat(page, 'Atlas');
    await send(page, `call:fs_read ${JSON.stringify({ path: join(ctx.workDir, 'hello.txt') })}`);
    await page.getByText(/tool said: .*hello from the smoke run/).first().waitFor(T);
    if (await page.getByText(/step failed/i).first().isVisible().catch(() => false)) throw new Error('a tool step failed: ' + ((await page.getByText(/tool said/).last().textContent()) ?? ''));
  });
  await run(s, 'a tool that needs approval shows the card; Allow works', async (page) => {
    await send(page, `call:fs_write ${JSON.stringify({ path: join(ctx.workDir, 'smoke-out.txt'), content: 'hi' })}`);
    const allow = page.getByRole('button', { name: /allow once|^allow/i }).first();
    await allow.waitFor(T);
    await allow.click();
    await page.getByText(/tool said: wrote 2 bytes/).first().waitFor(T);
    if (await page.getByText(/step failed/i).first().isVisible().catch(() => false)) throw new Error('a tool step failed after Allow');
  });
  await run(s, 'bot panel opens with its sections', async (page) => {
    await page.getByTestId('bot-chip').click();
    await page.getByTestId('bot-panel').waitFor(T);
  });
  await run(s, 'edit the bot (name, permission) and save twice in a row', async (page) => {
    for (const [name, preset] of [['Atlas Two', 'full'], ['Atlas Three', 'read-and-draft']]) {
      await page.getByTestId('bot-panel-edit').click();
      await page.getByTestId('bot-edit-name').fill(name);
      await page.getByText('Permissions', { exact: true }).first().click();
      await page.getByTestId(`bot-edit-preset-${preset}`).click();
      await page.getByTestId('bot-edit-save').click();
      await page.getByTestId('bot-edit-dialog').waitFor({ state: 'hidden', timeout: 15000 });
    }
    await page.getByText('Atlas Three').first().waitFor(T);
    // What the daemon stored, not just what the screen shows.
    const bots = await (await fetch(`${ctx.base}/v1/bots`, { headers: { authorization: `Bearer ${ctx.token}` } })).json();
    const atlas = bots.find((x) => x.name === 'Atlas Three');
    if (!atlas) throw new Error('the daemon does not have the renamed bot');
    if (!JSON.stringify(atlas.config).includes('read-and-draft')) throw new Error('the permission level was not stored');
  });
  await run(s, '@OtherBot mention is answered as the other bot', async (page) => {
    await send(page, '@Nova say hi');
    await page.getByTestId('guest-author').first().waitFor(T);
  });
  await run(s, 'Settings > Bots & models lists providers and the CLI engines section renders', async (page) => {
    await page.getByRole('button', { name: 'Settings', exact: true }).click();
    await page.getByTestId('settings-nav-models').click();
    await page.getByTestId('api-key-paste').waitFor(T);
    await page.getByTestId('detected-accounts-block').waitFor(T);
    await page.getByText('stub', { exact: false }).first().waitFor(T);
    await page.keyboard.press('Escape');
  });
  await run(s, 'Apps modal opens at once and shows tiles', async (page) => {
    await page.getByTestId('open-apps').click();
    // The modal opens before the daemon's catalog has loaded.
    await page.getByRole('dialog', { name: 'Apps' }).waitFor({ timeout: 4000 });
    await page.getByTestId('apps-grid').waitFor(T);
  });
  if (await s.page.getByRole('dialog', { name: 'Apps' }).isVisible()) await s.page.keyboard.press('Escape');
  await run(s, 'add-server wizard opens and validates input without SSH', async (page) => {
    await page.getByTestId('computer-picker').click();
    await page.getByTestId('computer-add').click();
    await page.getByTestId('add-server').waitFor(T);
    const start = page.getByTestId('add-server-start');
    if (await start.isEnabled()) throw new Error('Start is enabled with an empty host');
    await page.getByTestId('add-server-host').fill('not a host !!');
    // The daemon judges the name: a bad one must come back as a human sentence, with no SSH attempt.
    await start.click();
    const alert = page.getByTestId('add-server').getByRole('alert').first();
    await alert.waitFor(T);
    const text = ((await alert.textContent()) ?? '').trim();
    if (!text || /error:|panic|ssh:|exception|\bat \w+\./i.test(text)) throw new Error(`not a human error: "${text}"`);
    await page.keyboard.press('Escape');
  }, { expectError: true });
  // ---- permissions and screen control
  const auth = (extra = {}) => ({ authorization: `Bearer ${ctx.token}`, 'content-type': 'application/json', 'x-praxi-owner-key': ctx.ownerKey, ...extra });
  const sendLines = async (page, lines) => {
    // A run that is still finishing would queue the next message instead of starting it.
    await page.getByRole('button', { name: /^stop$/i }).waitFor({ state: 'hidden', timeout: 90000 }).catch(() => {});
    const c = composer(page);
    await c.fill(lines.join('\n'));
    // The composer ignores Enter while the previous send is still settling (its Send button is disabled
    // then and the keystroke would be dropped): wait for it to be idle, send, wait for the box to empty.
    await page.getByRole('button', { name: /^send$/i }).and(page.locator(':enabled')).waitFor({ timeout: 90000 });
    await c.press('Enter');
    await page.waitForFunction(() => { const el = document.querySelector('textarea[role="combobox"]'); return !el || el.value === ''; }, null, { timeout: 30000 });
  };
  const cards = (page) => page.getByTestId('approval-card');
  await run(s, 'Full level: a read, then shell_run with no untrusted content, asks nothing', async (page) => {
    // Always switch: the previous flows left another bot's chat open.
    await page.locator('[class*="railItem"]', { hasText: 'Fulla' }).first().click();
    await openFirstChat(page);
    await sendLines(page, ['call:schedule_list {}', `call:shell_run ${JSON.stringify({ command: 'echo smoke-one' })}`]);
    // A card would hold the run: the reply of the last step only arrives when nothing asked.
    await page.getByText(/tool said: .*smoke-one/).first().waitFor(SLOW);
    if ((await cards(page).count()) > 0) throw new Error('an approval card appeared although nothing untrusted was read');
  });
  await run(s, 'after an untrusted read one card offers "This chat"; the next shell call does not ask', async (page) => {
    await sendLines(page, ['call:web_search {"query":"smoke untrusted"}', `call:shell_run ${JSON.stringify({ command: 'echo smoke-two' })}`]);
    const card = cards(page).first();
    await card.waitFor(T);
    const why = ((await card.getByTestId('approval-why').textContent().catch(() => '')) ?? '').trim();
    if (!/untrusted/i.test(why)) throw new Error(`the card does not say why: "${why}"`);
    await card.getByRole('button', { name: /this chat/i }).click();
    await page.getByText(/tool said: .*smoke-two/).first().waitFor(SLOW);
    await sendLines(page, [`call:shell_run ${JSON.stringify({ command: 'echo smoke-three' })}`]);
    await page.getByText(/tool said: .*smoke-three/).first().waitFor(SLOW);
    // Answered cards stay in the timeline; only a card that still waits has its buttons.
    if ((await page.getByRole('button', { name: /allow once/i }).count()) > 0) throw new Error('the second shell call asked again after "This chat"');
  });
  await run(s, 'human-driving latch: banner, "Give control back", then an observation tool works', async (page) => {
    // A fresh page: with the live-view side panel open the viewer shows its own controls instead of the banner.
    await page.reload();
    await page.locator('[class*="railItem"]', { hasText: 'Fulla' }).first().waitFor(T);
    // The desktop app reports it with PUT /v1/control/state (its heartbeat); the real app's own idle
    // heartbeat (the shim's bridge here) would overwrite a single call, so repeat it like the app does.
    // The latch is another reporter's claim: an ownerless human_driving ends the moment the daemon hears
    // "idle" from anyone, and the page's bridge heartbeats idle every few seconds. Its PUTs are swallowed
    // for this step, otherwise they race the stand-in below and the page may never see the latch.
    await page.route('**/v1/control/state', (route) => (route.request().method() === 'PUT'
      ? route.fulfill({ status: 200, contentType: 'application/json', body: '{}' }) : route.continue()));
    const heartbeat = () => fetch(`${ctx.base}/v1/control/state`, { method: 'PUT', headers: auth(), body: JSON.stringify({ mode: 'human_driving' }) }).then((r) => r.ok);
    if (!(await heartbeat())) throw new Error('PUT /v1/control/state was refused');
    const beat = setInterval(() => void heartbeat().catch(() => {}), 150);
    const banner = page.getByTestId('human-driving-banner');
    try {
      await banner.waitFor(T).catch(async (e) => {
        const state = await (await fetch(`${ctx.base}/v1/control/state`, { headers: auth() })).text();
        throw new Error(`no banner; daemon says ${state}; ${String(e.message).slice(0, 60)}`);
      });
    } finally {
      clearInterval(beat);
    }
    if (!/You are driving/.test((await banner.textContent()) ?? '')) throw new Error('banner text is not the human-driving one');
    await banner.getByRole('button', { name: /give control back/i }).click();
    await banner.waitFor({ state: 'hidden', timeout: 15000 });
    await page.unroute('**/v1/control/state');
    await page.locator('[class*="railItem"]', { hasText: 'Fulla' }).first().click();
    await openFirstChat(page);
    await sendLines(page, ['call:desktop_list_apps {}']);
    await page.getByText(/tool said:/).last().waitFor(T);
    const said = (await page.getByText(/tool said:/).last().textContent()) ?? '';
    if (/observation paused|controlling the screen/i.test(said)) throw new Error(`observation still paused after giving control back: ${said.slice(0, 160)}`);
  });
  await run(s, 'production origins: preflight, health and an owner call answer for the Tauri webview', async () => {
    // The app's webview is `tauri://localhost` (macOS, Linux) or `http(s)://tauri.localhost` (Windows),
    // never the vite origin the rest of this run uses: the daemon must answer them, with the owner header.
    for (const origin of ['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost']) {
      const pre = await fetch(`${ctx.base}/v1/bots`, { method: 'OPTIONS', headers: { origin, 'access-control-request-method': 'POST', 'access-control-request-headers': 'authorization,content-type,x-praxi-owner-key' } });
      const allowed = (pre.headers.get('access-control-allow-headers') ?? '').toLowerCase();
      if (pre.status !== 204 || pre.headers.get('access-control-allow-origin') !== origin || !allowed.includes('x-praxi-owner-key') || !allowed.includes('authorization')) {
        throw new Error(`${origin}: preflight ${pre.status}, allow-origin ${pre.headers.get('access-control-allow-origin')}, allow-headers "${allowed}"`);
      }
      const health = await fetch(`${ctx.base}/v1/health`, { headers: { origin } });
      if (health.status !== 200 || health.headers.get('access-control-allow-origin') !== origin || (await health.json())?.status !== 'ok') throw new Error(`${origin}: health route`);
      const owned = await fetch(`${ctx.base}/v1/bots`, { headers: auth({ origin }) });
      if (owned.status !== 200 || owned.headers.get('access-control-allow-origin') !== origin) throw new Error(`${origin}: GET /v1/bots with the owner key answered ${owned.status}`);
    }
    const foreign = await fetch(`${ctx.base}/v1/bots`, { headers: auth({ origin: 'http://evil.example' }) });
    if (foreign.status !== 403) throw new Error(`a foreign origin got ${foreign.status}, not 403`);
  });
  await run(s, 'owner routes refuse the general token alone and accept the owner key', async () => {
    // The UI flows above already did these things with the owner key; here the same routes are
    // called the way a CLI engine or a script holding only the API token would call them.
    const post = (path, body, owner) => fetch(`${ctx.base}${path}`, {
      method: 'POST',
      headers: { authorization: `Bearer ${ctx.token}`, 'content-type': 'application/json', ...(owner ? { 'x-praxi-owner-key': ctx.ownerKey } : {}) },
      body: JSON.stringify(body)
    });
    const ghost = '00000000-0000-0000-0000-000000000001';
    for (const [path, body] of [
      [`/v1/approvals/${ghost}`, { decision: 'approve', scope: 'always' }],
      ['/v1/providers/cli/enable', { id: 'claude' }],
      ['/v1/secrets/smoke/extra-provider-key', { value: 'k' }]
    ]) {
      const bare = await post(path, body, false);
      const code = (await bare.json().catch(() => ({})))?.error?.code;
      if (bare.status !== 403 || code !== 'owner_key_required') throw new Error(`${path}: the general token alone got ${bare.status} ${code}`);
      const owner = await post(path, body, true);
      const ownerCode = (await owner.json().catch(() => ({})))?.error?.code;
      if (ownerCode === 'owner_key_required') throw new Error(`${path}: refused even with the owner key`);
    }
  });
  // ---- Stop must work every time: press Stop in the real UI, then ask the daemon.
  const api = (path, init = {}) => fetch(`${ctx.base}/v1${path}`, { ...init, headers: auth(init.headers) });
  const runsNow = async () => (await api('/runs')).json();
  const isTerminal = (st) => ['succeeded', 'failed', 'cancelled'].includes(st);
  const until = async (what, fn, ms) => {
    const end = Date.now() + ms;
    for (;;) {
      const v = await fn();
      if (v) return v;
      if (Date.now() > end) throw new Error(`timed out (${ms} ms): ${what}`);
      await new Promise((r) => setTimeout(r, 50));
    }
  };
  const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
  const holdScript = join(ctx.workDir, 'hold.js');
  writeFileSync(holdScript, "require('fs').writeFileSync(process.argv[2], String(process.pid)); setInterval(() => {}, 1000);\n");
  const STOP_WITHIN = 3000;
  const composerEmpty = (page) => page.waitForFunction(() => { const el = document.querySelector('textarea[role="combobox"]'); return !el || el.value === ''; }, null, { timeout: 30000 });
  const openCount = () => ctx.stubLog.filter((l) => l === 'MODEL_OPEN').length;
  const abortCount = () => ctx.stubLog.filter((l) => l === 'MODEL_ABORTED').length;
  /** Press Stop and verify daemon cancellation, settled UI and an enabled composer. */
  const stopAndCheck = async (page, before, { appearWithin = 15000, cancelWithin = STOP_WITHIN, afterClick } = {}) => {
    // The send button is the Stop button (a rounded square) while the bot works and the box is empty.
    const stopBtn = page.locator('[data-testid="send-button"][data-mode="stop"]');
    await stopBtn.waitFor({ timeout: appearWithin });
    await stopBtn.click();
    await afterClick?.();
    const t0 = Date.now();
    await until('run reaches "cancelled" in the daemon', async () => (await runsNow()).find((r) => !before.has(r.id) && !r.parent_run_id && r.state === 'cancelled'), cancelWithin);
    await page.getByTestId('run-status').waitFor({ state: 'hidden', timeout: Math.max(cancelWithin - (Date.now() - t0), 500) });
    if (await stopBtn.isVisible()) throw new Error('the Stop button is still shown after the run stopped');
    const c = composer(page);
    await c.fill('x');
    await page.getByRole('button', { name: /^send$/i }).and(page.locator(':enabled')).waitFor({ timeout: 3000 });
    await c.fill('');
  };
  const gotoBot = async (page, name) => {
    await page.locator('[class*="railItem"]', { hasText: name }).first().click();
    await openFirstChat(page);
    await page.getByTestId('run-status').waitFor({ state: 'hidden', timeout: 30000 }).catch(() => {});
  };
  await run(s, 'Stop (a): during a slow model response the request is aborted and the run is cancelled', async (page) => {
    await gotoBot(page, 'Fulla');
    const before = new Set((await runsNow()).map((r) => r.id));
    const open0 = openCount();
    const abort0 = abortCount();
    await send(page, 'slow:60000');
    await until('provider request open', () => openCount() > open0, 20000);
    await stopAndCheck(page, before);
    await until('provider request aborted', () => abortCount() > abort0, STOP_WITHIN);
  });
  await run(s, 'Stop (b): during a long shell_run the process tree is gone and nothing else runs', async (page) => {
    await gotoBot(page, 'Fulla');
    const before = new Set((await runsNow()).map((r) => r.id));
    const pidFile = join(ctx.workDir, 'hold.pid');
    rmSync(pidFile, { force: true });
    const windows = process.platform === 'win32';
    const quote = (path) => `'${path.replaceAll("'", windows ? "''" : "'\"'\"'")}'`;
    const command = `${windows ? '&' : 'exec'} ${[process.execPath, holdScript, pidFile].map(quote).join(' ')}`;
    await sendLines(page, [`call:shell_run ${JSON.stringify({ command })}`, `call:shell_run ${JSON.stringify({ command: 'echo must-not-run' })}`]);
    // Depending on the chat's prior grant, the fixture command can start or ask first.
    const allow = cards(page).getByRole('button', { name: /^allow once$/i });
    const readPid = () => existsSync(pidFile) ? Number(readFileSync(pidFile, 'utf8')) : 0;
    const started = await until('hold process starts or asks for approval', async () => readPid() || await allow.isVisible(), 60000);
    if (started === true) await allow.click();
    const pid = typeof started === 'number' ? started : await until('hold process started', readPid, 60000);
    await stopAndCheck(page, before);
    await until('hold process gone', () => !alive(pid), STOP_WITHIN);
    await new Promise((r) => setTimeout(r, 1000));
    if ((await page.getByText(/tool said: .*must-not-run/).count()) > 0) throw new Error('the second tool call ran after Stop');
  });
  await run(s, 'Stop (c): while an approval card is open the run is cancelled and the card is dead', async (page) => {
    await gotoBot(page, 'Nova');
    const before = new Set((await runsNow()).map((r) => r.id));
    const target = join(ctx.workDir, 'stop-approval.txt');
    await send(page, `call:fs_write ${JSON.stringify({ path: target, content: 'hi' })}`);
    await cards(page).first().waitFor(T);
    await stopAndCheck(page, before);
    if ((await page.getByRole('button', { name: /allow once|^allow/i }).count()) > 0) throw new Error('an approval card can still be answered after Stop');
    if (existsSync(target)) throw new Error('the gated tool ran');
  });
  await run(s, 'Stop (d): pressed right after Send, before the first event, still stops the run', async (page) => {
    await gotoBot(page, 'Fulla');
    const before = new Set((await runsNow()).map((r) => r.id));
    const sendUrl = `${ctx.base}/v1/chats/*/messages`;
    const release = Promise.withResolvers();
    const holdSend = async (route) => {
      if (route.request().method() === 'POST') await release.promise;
      await route.fallback();
    };
    // Hold the real send request until the click so this always tests Stop before the first run event.
    await page.route(sendUrl, holdSend);
    try {
      const c = composer(page);
      await c.fill('slow:60000');
      await c.press('Enter');
      await stopAndCheck(page, before, {
        appearWithin: 1500,
        // Before the first event, the UI must receive the send response before it can cancel by run ID.
        cancelWithin: 15000,
        afterClick: async () => {
          release.resolve();
          // A pre-send Stop can be applied only once the daemon has registered the held request.
          await until('the daemon registers the held send', async () => (await runsNow()).find((r) => !before.has(r.id) && !r.parent_run_id), 15000);
        }
      });
    } finally {
      release.resolve();
      await page.unroute(sendUrl, holdSend);
    }
    await new Promise((r) => setTimeout(r, 1000));
    const live = (await runsNow()).filter((r) => !before.has(r.id) && !isTerminal(r.state));
    if (live.length) throw new Error(`a run is still live after Stop: ${live.map((r) => r.state).join(',')}`);
  });
  await run(s, 'Stop (e): with a second message queued, Stop ends the run and the queued message does not start', async (page) => {
    await gotoBot(page, 'Fulla');
    const before = new Set((await runsNow()).map((r) => r.id));
    const open0 = openCount();
    await send(page, 'slow:60000');
    await until('provider request open', () => openCount() > open0, 20000);
    await composerEmpty(page);
    await send(page, 'echo second queued message');
    await composerEmpty(page);
    await stopAndCheck(page, before);
    await new Promise((r) => setTimeout(r, 2000));
    const started = (await runsNow()).filter((r) => !before.has(r.id) && !r.parent_run_id);
    if (started.length !== 1) throw new Error(`Stop left ${started.length} runs (${started.map((r) => r.state).join(',')}); the queued message started a new turn`);
    if (openCount() !== open0 + 1) throw new Error('a further model request was made after Stop');
  });
  await s.context.close();

  // ---- a coding CLI that only works with its own tools (a fake `cursor-agent` on PATH speaking ACP; the harness
  // cannot use real subscriptions): pick it in the wizard, give the one consent, the reply arrives.
  const cli = await newSession(ctx, { onboarded: false });
  await run(cli, 'a CLI model that needs its own tools: one consent step, then its reply arrives', async (page) => {
    await page.goto(ctx.uiUrl);
    await page.getByTestId('onboarding-view').waitFor(T);
    await page.getByTestId('fr-cli-cursor').click();
    await page.getByTestId('fr-connected').waitFor({ timeout: 60000 });
    await page.getByTestId('fr-next').click();
    await page.getByTestId('fr-level-balanced').waitFor(T);
    await page.getByTestId('fr-next').click();
    await page.getByTestId('new-bot-name').fill('Cliy');
    await page.getByTestId('fr-next').click();
    // The consent is asked here, with Ask preselected, not as an error after the first message.
    await page.getByTestId('engine-consent').waitFor(T);
    if (!(await page.getByTestId('engine-consent-level-ask').locator('input').isChecked())) throw new Error('Ask is not preselected');
    await page.getByTestId('engine-consent-allow').click();
    await page.getByTestId('bot-empty-state').waitFor(T);
    const bots = await (await fetch(`${ctx.base}/v1/bots`, { headers: { authorization: `Bearer ${ctx.token}` } })).json();
    const engine = bots.find((x) => x.name === 'Cliy')?.config?.engine;
    if (engine?.mode !== 'native_tools' || engine?.approval_level !== 'ask') throw new Error(`the bot was stored as ${JSON.stringify(engine)}`);
    await openFirstChat(page);
    await send(page, 'hello cli');
    await page.getByText(/native acp/).first().waitFor(SLOW);
    // The fake CLI asks to run a command at the Ask level: the question is a Praxi approval card.
    const allow = page.getByRole('button', { name: /allow once|^allow/i }).first();
    await allow.waitFor(T);
    await allow.click();
  });
  await cli.context.close();
  return results;
}
