import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";
import { MockApiClient } from "../api/mock.ts";
import type { Bot } from "../api/types.ts";
import type { FlowSeed } from "./ConnectMail.tsx";
import type { GuideSaved, GuideTest } from "../lib/mailGuide.ts";

let server: ViteDevServer;
let Flow: ComponentType<Record<string, unknown>>;
let i18n: { changeLanguage: (lng: string) => Promise<unknown> };
let bots: Bot[];

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null, hmr: false },
    ssr: { noExternal: ["ui", "@tauri-apps/api"] }
  });
  ({ i18n } = await server.ssrLoadModule("/src/i18n/index.ts"));
  ({ ConnectMailFlow: Flow } = await server.ssrLoadModule("/src/views/ConnectMail.tsx"));
  bots = await new MockApiClient().listBots();
});
after(async () => {
  await server?.close();
});

const noop = () => undefined;
const render = (seed: FlowSeed, list: Bot[] = bots) =>
  renderToStaticMarkup(createElement(Flow, { client: null, bots: list, seed, onClose: noop }));

/** With a client, so a button is disabled only for its own reason, not for lack of a daemon. */
const renderLive = (seed: FlowSeed, list: Bot[] = bots) =>
  renderToStaticMarkup(createElement(Flow, { client: new MockApiClient(), bots: list, seed, onClose: noop }));

const badMail: GuideTest = {
  mail: { state: "failed", report: { imap: "auth_failed", smtp: "auth_failed", ok: false } },
  calendar: null
};

test("step 1: the provider is read from the address and the owner can change it", () => {
  const html = render({ address: "me@icloud.com" });
  assert.match(html, /data-step="address"/);
  assert.match(html, /This looks like iCloud/);
  assert.match(html, /data-testid="connect-mail-provider-select"/);
  assert.match(html, /Gmail or Google Workspace/);
  assert.match(html, /Outlook or Microsoft 365/);
  // Nothing about the provider before there is an address.
  assert.ok(!render({}).includes("connect-mail-provider-select"));
});

test("step 1: an address on the owner's own domain can be set to Google Workspace or Microsoft 365", () => {
  const html = render({ address: "me@acme.example" });
  assert.match(html, /cannot tell who runs acme\.example/);
  const workspace = render({ address: "me@acme.example", provider: "google" });
  assert.match(workspace, /Connecting|Gmail or Google Workspace/);
});

test("iCloud: two lines of instructions, a direct link, one app password and a read-only default", () => {
  const html = render({ step: "setup", address: "me@icloud.com" });
  assert.match(html, /Make an app password/);
  assert.match(html, /App-Specific Passwords/);
  assert.match(html, /Open the iCloud page/);
  assert.match(html, /type="password"/);
  assert.match(html, /data-testid="connect-mail-test"/);
  // Read and draft only unless the owner ticks the second opt-in; the box starts unticked.
  assert.match(html, /Read and draft only/);
  assert.match(html, /data-testid="connect-mail-allow-send"/);
  assert.ok(!/data-testid="connect-mail-allow-send"[^>]*checked/.test(html) && !/checked=""[^>]*data-testid="connect-mail-allow-send"/.test(html));
  // One password serves the calendar too: no second password field.
  assert.equal((html.match(/type="password"/g) ?? []).length, 1);
  assert.match(html, /Adding or changing events follows the bot/);
});

test("with exactly one bot, it is the one picked; with several, nobody is picked for the owner", () => {
  const one = render({ step: "setup", address: "me@icloud.com" }, [bots[0]]);
  assert.match(one, /checked/);
  const many = render({ step: "setup", address: "me@icloud.com" });
  assert.match(many, /data-testid="bot-access-picker"/);
});

test("Google: sign-in is the first choice, the app password is the mail-only alternative", () => {
  const html = render({ step: "setup", address: "me@gmail.com" });
  assert.match(html, /Sign in with Google \(recommended\)/);
  assert.match(html, /App password/);
  assert.match(html, /data-testid="connect-mail-signin-google"/);
  assert.match(html, /data-testid="connect-mail-open-google"/);
  // The sign-in path needs no password field and no live-test button.
  assert.ok(!html.includes('type="password"'));
  assert.ok(!html.includes("connect-mail-test"));
  assert.match(html, /data-testid="connect-mail-finish"/);
  const alt = render({ step: "setup", address: "me@gmail.com", googlePath: "app_password", wantCalendar: false });
  assert.match(alt, /16 letters/);
  assert.match(alt, /myaccount\.google\.com|Open the Gmail or Google Workspace page/);
  assert.ok(!alt.includes("connect-mail-signin-google"));
});

test("Outlook: no app password, only Microsoft's own sign-in, with the reason said plainly", () => {
  const html = render({ step: "setup", address: "me@outlook.com" });
  assert.match(html, /data-testid="connect-mail-signin-microsoft"/);
  assert.match(html, /no longer accepts app passwords/);
  assert.ok(!html.includes('type="password"'));
});

test("Yahoo: mail only, and the calendar is explained rather than offered", () => {
  const html = render({ step: "setup", address: "me@yahoo.com" });
  assert.match(html, /no calendar connection Praxi can use/);
  assert.match(html, /Generate app password/);
});

test("another provider: a calendar server field, and server details to fix a wrong guess", () => {
  const html = render({ step: "setup", address: "me@acme.example" });
  assert.match(html, /Calendar server address/);
  assert.match(html, /data-testid="connect-mail-more"/);
  assert.match(html, /imap\.acme\.example/);
  assert.match(html, /smtp\.acme\.example/);
});

test("a failed login is explained in plain words with the provider's own fix, never a raw status", () => {
  const html = render({ step: "setup", address: "me@icloud.com", password: "x", test: badMail });
  assert.match(html, /data-testid="connect-mail-results"/);
  assert.match(html, /Mail does not work yet/);
  assert.match(html, /Apple refused the login\. Use an app-specific password/);
  assert.ok(!html.includes("auth_failed"));
  // Nothing connects until something passed.
  assert.ok(!html.includes("connect-mail-connect"));
});

test("a guessed server that does not answer opens the server details", () => {
  const test_: GuideTest = {
    mail: { state: "failed", report: { imap: "unreachable", smtp: "unreachable", ok: false } },
    calendar: null
  };
  const html = render({ step: "setup", address: "me@acme.example", password: "x", test: test_ });
  assert.match(html, /We guessed the server names/);
  assert.match(html, /<details[^>]*open/);
});

test("when mail works and the calendar does not, the owner can connect what works", () => {
  const half: GuideTest = {
    mail: { state: "ok", report: { imap: "ok", smtp: "ok", ok: true } },
    calendar: {
      state: "failed",
      report: { ok: false, calendars: [], scheduling: false, error: "The server refused the login. Check the user name and the app password." }
    }
  };
  const html = render({ step: "setup", address: "me@icloud.com", password: "x", test: half });
  assert.match(html, /Mail works/);
  assert.match(html, /Calendar does not work yet/);
  assert.match(html, /same app-specific password/);
  assert.match(html, /Connect what works/);
});

test("when everything works the button says Connect, and several calendars can be ticked", () => {
  const good: GuideTest = {
    mail: { state: "ok", report: { imap: "ok", smtp: "ok", ok: true } },
    calendar: {
      state: "ok",
      report: {
        ok: true,
        scheduling: true,
        error: null,
        calendars: [
          { name: "Home", url: "https://p01-caldav.icloud.com/1/home/" },
          { name: "Work", url: "https://p01-caldav.icloud.com/1/work/" }
        ]
      }
    }
  };
  const html = render({ step: "setup", address: "me@icloud.com", password: "x", test: good });
  assert.match(html, />Connect</);
  assert.match(html, /Which calendars should the bot see/);
  assert.match(html, /Home/);
  assert.match(html, /The bot will only read and draft/);
});

test("the last step says what is connected, that sending is off, and how to take access back", () => {
  const saved: GuideSaved = { mail: { name: "icloud" }, calendars: ["icloud-home"], failed: [] };
  const html = render({ step: "done", address: "me@icloud.com", provider: "icloud", saved });
  assert.match(html, /All set/);
  assert.match(html, /Mail is connected as icloud/);
  assert.match(html, /1 calendar is connected/);
  assert.match(html, /can read and draft mail, but not send it/);
  assert.match(html, /Disconnect everything/);
  assert.match(html, /Open the iCloud access page/);
  const sending = render({ step: "done", address: "me@icloud.com", provider: "icloud", saved, allowSend: true });
  assert.match(sending, /Sending is on/);
});

const testButton = (html: string) => /<button[^>]*data-testid="connect-mail-test"[^>]*>/.exec(html)?.[0] ?? "";

test("another provider: the guessed servers are shown and nothing is tested until the owner confirms them", () => {
  const html = renderLive(
    { step: "setup", address: "me@acme.example", password: "x", calendarServer: "dav.acme.example" },
    [bots[0]]
  );
  assert.match(html, /data-testid="connect-mail-approve-box"/);
  assert.match(html, /Check where your password will go/);
  assert.match(html, /Incoming mail \(IMAP\): imap\.acme\.example, port 993/);
  assert.match(html, /Outgoing mail \(SMTP\): smtp\.acme\.example, port 465/);
  assert.match(html, /Calendar \(CalDAV\): https:\/\/dav\.acme\.example\//);
  assert.match(html, /data-testid="connect-mail-approve"/);
  assert.ok(!/data-testid="connect-mail-approve"[^>]*checked/.test(html));
  assert.match(testButton(html), /disabled/, "the test button waits for the owner's confirmation");
  assert.match(html, /Confirm the servers above to test the connection/);
  // The server details are open from the start for an unknown provider.
  assert.match(html, /<details[^>]*open/);
});

test("a known provider goes straight through: no confirmation, the test button is ready", () => {
  for (const address of ["me@icloud.com", "me@fastmail.com", "me@yahoo.com"]) {
    const html = renderLive({ step: "setup", address, password: "x", wantCalendar: false }, [bots[0]]);
    assert.ok(!html.includes("connect-mail-approve-box"), address);
    assert.ok(!/disabled/.test(testButton(html)), address);
  }
  const gmail = renderLive(
    { step: "setup", address: "me@gmail.com", googlePath: "app_password", password: "x", wantCalendar: false },
    [bots[0]]
  );
  assert.ok(!gmail.includes("connect-mail-approve-box"));
  assert.ok(!/disabled/.test(testButton(gmail)));
});

test("a calendar address that is not https, or that names a private place, blocks the test and says why", () => {
  const base = { step: "setup", address: "me@acme.example", password: "x" } as const;
  const bot = [bots[0]];
  const http = renderLive({ ...base, calendarServer: "http://dav.acme.example/" }, bot);
  assert.match(http, /data-testid="connect-mail-problems"/);
  assert.match(http, /must start with https:\/\//);
  assert.match(testButton(http), /disabled/);
  assert.match(render({ ...base, calendarServer: "ftp://dav.acme.example/" }), /must start with https:\/\//);
  assert.match(render({ ...base, calendarServer: "https://192.168.1.5/dav/" }), /not an IP address/);
  assert.match(render({ ...base, calendarServer: "https://me:pw@dav.acme.example/" }), /without a login or password/);
});

test("raw daemon text is never shown: only the fix for each cause, in plain words", () => {
  const leaky = "boom https://evil.example/x?token=abc user:hunter2@evil.example " + "x".repeat(2000);
  const failedCalendar: GuideTest = {
    mail: { state: "error", error: Object.assign(new Error(leaky), { status: 422, code: "bad_email_config" }) },
    calendar: { state: "failed", report: { ok: false, calendars: [], scheduling: false, error: leaky } }
  };
  const html = render({ step: "setup", address: "me@icloud.com", password: "x", test: failedCalendar });
  for (const leak of ["evil.example", "hunter2", "token=abc", "boom", "xxxx"]) {
    assert.ok(!html.includes(leak), `${leak} was shown`);
  }
  assert.match(html, /Praxi could not use these details/);
  assert.match(html, /The calendar server did not accept the request/);
});

test("a test that found hundreds of calendars lists at most 20, and only https ones", () => {
  const many: GuideTest = {
    mail: null,
    calendar: {
      state: "ok",
      report: {
        ok: true,
        scheduling: false,
        error: null,
        calendars: [
          ...Array.from({ length: 300 }, (_, i) => ({ name: `Cal-${i}-`, url: `https://dav.example.com/${i}/` })),
          { name: "Plain", url: "http://dav.example.com/plain/" }
        ]
      }
    }
  };
  const html = render({ step: "setup", address: "me@icloud.com", password: "x", test: many });
  assert.equal((html.match(/Cal-\d+-/g) ?? []).length, 20);
  assert.ok(!html.includes("Plain"));
  assert.match(html, /Found 20 calendars/);
});

test("no screen shows an untranslated key, in English or Turkish", async () => {
  const seeds: FlowSeed[] = [
    {},
    { address: "me@icloud.com" },
    { address: "me@acme.example" },
    { step: "setup", address: "me@icloud.com" },
    { step: "setup", address: "me@gmail.com" },
    { step: "setup", address: "me@gmail.com", googlePath: "app_password" },
    { step: "setup", address: "me@outlook.com" },
    { step: "setup", address: "me@yahoo.com" },
    { step: "setup", address: "me@fastmail.com", password: "x", test: badMail },
    { step: "setup", address: "me@acme.example", password: "x", test: badMail },
    { step: "setup", address: "me@acme.example", password: "x", calendarServer: "dav.acme.example" },
    { step: "setup", address: "me@acme.example", password: "x", calendarServer: "http://192.168.1.2/" },
    { step: "done", address: "me@icloud.com", provider: "icloud", saved: { mail: { name: "icloud" }, calendars: ["icloud"], failed: [] } },
    { step: "done", address: "me@gmail.com", provider: "google", saved: { mail: null, calendars: [], failed: [] }, signedIn: { google: true } }
  ];
  for (const lng of ["en", "tr"]) {
    await i18n.changeLanguage(lng);
    for (const seed of seeds) {
      const html = render(seed);
      assert.ok(!html.includes("settingsModal.connectMail"), `${lng} ${JSON.stringify(seed)} shows a raw key`);
      assert.ok(!/\{\{/.test(html), `${lng} ${JSON.stringify(seed)} shows a raw placeholder`);
    }
  }
  const tr = render({ step: "setup", address: "me@icloud.com" });
  assert.match(tr, /uygulama parolası/i);
  await i18n.changeLanguage("en");
});
