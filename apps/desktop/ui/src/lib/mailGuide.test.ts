import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import type { CalendarTestReport, EmailTestReport, McpServer } from "../api/types.ts";
import {
  allPassed,
  anyPassed,
  calendarAccountName,
  calendarFixKey,
  calendarProblem,
  calendarServerProblem,
  calendarUrl,
  detectProvider,
  disconnectAll,
  domainOf,
  errorFixKey,
  foundCalendars,
  type GuidePlan,
  type GuideTest,
  googleSignedIn,
  isApproved,
  isPublicHostName,
  isServerProblem,
  loadConnected,
  mailFixKey,
  mailInput,
  mailServers,
  mailUsable,
  MAX_CALENDARS,
  nameBase,
  needsApproval,
  normaliseServerAddress,
  planProblems,
  PROVIDER_IDS,
  providerProfile,
  savePlan,
  sendModeOf,
  serversEdited,
  targetKey,
  testPlan,
  uniqueName
} from "./mailGuide.ts";

const read = (lng: string) =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${lng}.json`, import.meta.url), "utf8")) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

const ATLAS = "11111111-1111-4111-8111-111111111111";
const SECRET = "abcd efgh ijkl mnop";

function plan(over: Partial<GuidePlan> = {}): GuidePlan {
  return {
    provider: "icloud",
    address: "me@icloud.com",
    login: "",
    password: SECRET,
    mail: { on: true, servers: mailServers("icloud", "me@icloud.com"), bot: ATLAS, allowSend: false },
    calendar: { on: true, server: "" },
    ...over
  };
}

const goodMail: EmailTestReport = { imap: "ok", smtp: "ok", ok: true };
const goodCalendar: CalendarTestReport = {
  ok: true,
  scheduling: true,
  calendars: [
    { name: "Home", url: "https://p01-caldav.icloud.com/123/calendars/home/" },
    { name: "Work", url: "https://p01-caldav.icloud.com/123/calendars/work/" }
  ],
  error: null
};

test("the provider is read from the address", () => {
  const cases: Array<[string, string | null]> = [
    ["me@gmail.com", "google"],
    ["me@googlemail.com", "google"],
    ["me@outlook.com", "microsoft"],
    ["me@hotmail.com", "microsoft"],
    ["me@live.com", "microsoft"],
    ["me@icloud.com", "icloud"],
    ["me@me.com", "icloud"],
    ["me@yahoo.com", "yahoo"],
    ["me@yahoo.co.uk", "yahoo"],
    ["me@ymail.com", "yahoo"],
    ["me@fastmail.com", "fastmail"],
    ["me@FASTMAIL.FM", "fastmail"],
    ["me@acme.example", "other"],
    // Never a look-alike: the domain must be the provider's or one of its subdomains.
    ["me@notgmail.com", "other"],
    ["me@gmail.com.evil.example", "other"],
    ["me@evilicloud.com", "other"],
    ["not an address", null],
    ["", null],
    ["me@", null]
  ];
  for (const [address, expected] of cases) {
    assert.equal(detectProvider(address), expected, address);
  }
  assert.equal(domainOf(" Me@Example.COM "), "example.com");
});

test("every provider has a way to connect mail, and the easiest comes first", () => {
  for (const id of PROVIDER_IDS) {
    const p = providerProfile(id);
    assert.ok(p.mail.length > 0, id);
    assert.ok(p.calendar.length > 0, id);
  }
  // Google: official sign-in first, an app password as the mail-only alternative.
  assert.deepEqual(providerProfile("google").mail, ["google_sign_in", "app_password"]);
  assert.deepEqual(providerProfile("google").calendar, ["google_sign_in"]);
  // Microsoft turned off app passwords for mail: only its own sign-in is offered.
  assert.deepEqual(providerProfile("microsoft").mail, ["microsoft_sign_in"]);
  assert.equal(providerProfile("microsoft").appPasswordUrl, null);
  // App-password providers link straight to the page that makes one, over https.
  for (const id of ["google", "icloud", "yahoo", "fastmail"] as const) {
    assert.match(providerProfile(id).appPasswordUrl ?? "", /^https:\/\//, id);
  }
  // Yahoo has no calendar connection Praxi can offer.
  assert.deepEqual(providerProfile("yahoo").calendar, ["none"]);
});

test("servers: known providers use their own, an unknown one is guessed from the domain", () => {
  assert.equal(mailServers("icloud", "me@icloud.com").imapHost, "imap.mail.me.com");
  assert.equal(mailServers("yahoo", "me@yahoo.com").smtpPort, 465);
  assert.equal(mailServers("fastmail", "me@fastmail.com").imapHost, "imap.fastmail.com");
  const guess = mailServers("other", "me@acme.example");
  assert.equal(guess.imapHost, "imap.acme.example");
  assert.equal(guess.smtpHost, "smtp.acme.example");
  assert.equal(guess.imapPort, 993);
  assert.equal(guess.smtpSecurity, "implicit");
});

test("a new mailbox is read-and-draft only until the owner opts in to sending", () => {
  assert.equal(sendModeOf(false), "draft_only");
  assert.equal(sendModeOf(true), "ask");
  const body = mailInput(plan(), "icloud");
  assert.equal(body.send_mode, "draft_only");
  // Least privilege: bound to the one bot, shared with nobody, never everyone.
  assert.equal(body.bot, ATLAS);
  assert.deepEqual(body.shared_with, []);
  assert.equal(body.username, null);
  assert.equal(body.provider, "imap_smtp");
  const sends = mailInput(plan({ mail: { ...plan().mail!, allowSend: true } }), "icloud");
  assert.equal(sends.send_mode, "ask");
  // A login that differs from the address (an Apple ID) is sent as the username.
  assert.equal(mailInput(plan({ login: "appleid@example.org" }), "icloud").username, "appleid@example.org");
});

test("CalDAV addresses: iCloud and Fastmail are known, anything else is what the owner typed", () => {
  assert.equal(calendarUrl(plan()), "https://caldav.icloud.com/");
  assert.equal(
    calendarUrl(plan({ provider: "fastmail", address: "me@fastmail.com" })),
    "https://caldav.fastmail.com/dav/calendars/user/me%40fastmail.com/"
  );
  assert.equal(
    calendarUrl(plan({ provider: "other", address: "me@acme.example", calendar: { on: true, server: "dav.acme.example" } })),
    "https://dav.acme.example/"
  );
  assert.equal(normaliseServerAddress("  https://dav.acme.example/cal/ "), "https://dav.acme.example/cal/");
  assert.equal(normaliseServerAddress(""), "");
});

test("names stay short, valid and unique", () => {
  assert.equal(uniqueName("icloud", []), "icloud");
  assert.equal(uniqueName("icloud", ["icloud"]), "icloud-2");
  assert.equal(uniqueName("icloud", ["icloud", "icloud-2"]), "icloud-3");
  assert.equal(nameBase("google", "me@gmail.com"), "gmail");
  assert.equal(nameBase("other", "me@acme.example"), "acme");
  assert.equal(calendarAccountName("icloud", "Work Stuff!", [], true), "icloud-work-stuff");
  assert.equal(calendarAccountName("icloud", "Home", [], false), "icloud");
  assert.match(calendarAccountName("icloud", "x".repeat(100), [], true), /^[A-Za-z0-9_-]{1,40}$/);
});

test("every failure has a plain-words fix, in English and Turkish", () => {
  const en = read("en");
  const tr = read("tr");
  const keys = new Set<string>();
  for (const provider of PROVIDER_IDS) {
    for (const status of ["auth_failed", "unreachable", "tls", "protocol", "timeout"] as const) {
      const key = mailFixKey(provider, status);
      assert.ok(key, `${provider}/${status}`);
      keys.add(key);
    }
    for (const error of ["The server refused the login.", "not found", "could not reach", "redirect (301)", "boom"]) {
      keys.add(calendarFixKey(provider, error));
    }
  }
  assert.equal(mailFixKey("icloud", "ok"), null);
  for (const key of keys) {
    const full = `settingsModal.connectMail.fix.${key}`;
    for (const [lng, tree] of [["en", en], ["tr", tr]] as const) {
      const text = lookup(tree, full);
      assert.equal(typeof text, "string", `${lng} is missing ${full}`);
      assert.ok((text as string).length > 20, `${lng} ${full}`);
    }
  }
});

test("a wrong password gets the provider's own advice", () => {
  assert.equal(mailFixKey("icloud", "auth_failed"), "mail.auth.icloud");
  assert.equal(mailFixKey("google", "auth_failed"), "mail.auth.google");
  // A guessed server that does not answer says so, and points at the server fields.
  assert.equal(mailFixKey("other", "unreachable"), "mail.unreachableGuess");
  assert.equal(mailFixKey("icloud", "unreachable"), "mail.unreachable");
  assert.equal(isServerProblem("unreachable"), true);
  assert.equal(isServerProblem("auth_failed"), false);
  assert.equal(calendarProblem("The server refused the login. Check the user name."), "auth");
  assert.equal(calendarProblem("the server answered with a redirect (301 Moved)"), "redirect");
  assert.equal(calendarProblem("could not reach the server"), "unreachable");
  assert.equal(calendarFixKey("icloud", "refused the login"), "calendar.auth.icloud");
  assert.equal(calendarFixKey("other", "refused the login"), "calendar.auth.other");
});

test("the live test runs both parts, saves nothing and sends the password only in the write-only field", async () => {
  const seen: unknown[] = [];
  const api = {
    testEmailDraft: async (input: unknown) => {
      seen.push(input);
      return goodMail;
    },
    testCalendar: async (input: unknown) => {
      seen.push(input);
      return goodCalendar;
    }
  };
  const result = await testPlan(api, plan());
  assert.equal(result.mail?.state, "ok");
  assert.equal(result.calendar?.state, "ok");
  assert.ok(allPassed(result));
  assert.equal(seen.length, 2);
  // Neither result carries the password back.
  assert.ok(!JSON.stringify(result).includes(SECRET));
});

test("a part that is not wanted is not tested", async () => {
  let calls = 0;
  const api = {
    testEmailDraft: async () => {
      calls += 1;
      return goodMail;
    },
    testCalendar: async () => {
      calls += 1;
      return goodCalendar;
    }
  };
  const result = await testPlan(api, plan({ calendar: { on: false, server: "" } }));
  assert.equal(result.calendar, null);
  assert.equal(calls, 1);
});

test("a login the server refuses is a failed part with its status, and a daemon refusal is an error", async () => {
  const api = {
    testEmailDraft: async (): Promise<EmailTestReport> => ({ imap: "auth_failed", smtp: "auth_failed", ok: false }),
    testCalendar: async (): Promise<CalendarTestReport> => {
      throw new Error("calendar down");
    }
  };
  const result = await testPlan(api, plan());
  assert.equal(result.mail?.state, "failed");
  assert.equal(result.calendar?.state, "error");
  assert.ok(!allPassed(result));
  assert.ok(!anyPassed(result));
});

test("while the bot may not send, a refused outgoing server does not stop a read-only connection", async () => {
  const report: EmailTestReport = { imap: "ok", smtp: "auth_failed", ok: false };
  assert.equal(mailUsable(report, false), true);
  assert.equal(mailUsable(report, true), false);
  assert.equal(mailUsable({ imap: "auth_failed", smtp: "ok", ok: false }, false), false);
  const api = {
    testEmailDraft: async () => report,
    testCalendar: async () => goodCalendar
  };
  const readOnly = await testPlan(api, plan());
  assert.equal(readOnly.mail?.state, "ok");
  const sends = await testPlan(api, plan({ mail: { ...plan().mail!, allowSend: true } }));
  assert.equal(sends.mail?.state, "failed");
});

test("saving connects what passed: one mailbox and the ticked calendars, passwords only in write-only fields", async () => {
  const api = new MockApiClient();
  const before = (await api.listEmailAccounts()).length;
  const p = plan();
  const t: GuideTest = {
    mail: { state: "ok", report: goodMail },
    calendar: { state: "ok", report: goodCalendar }
  };
  const saved = await savePlan(api, p, t, { mail: (await api.listEmailAccounts()).map((a) => a.name), calendar: [] }, goodCalendar.calendars);
  assert.deepEqual(saved.mail, { name: "icloud" });
  assert.deepEqual(saved.calendars, ["icloud-home", "icloud-work"]);
  assert.deepEqual(saved.failed, []);

  const mailboxes = await api.listEmailAccounts();
  assert.equal(mailboxes.length, before + 1);
  const mine = mailboxes.find((a) => a.name === "icloud");
  assert.ok(mine);
  assert.equal(mine.password_set, true);
  assert.equal(mine.bot, ATLAS);
  // The secret is write-only: nothing that comes back (or that the flow returns) contains it.
  assert.ok(!JSON.stringify(mailboxes).includes(SECRET));
  const calendars = await api.listCalendarAccounts();
  assert.deepEqual(calendars.map((c) => c.name).sort(), ["icloud-home", "icloud-work"]);
  assert.ok(!JSON.stringify(calendars).includes(SECRET));
  assert.ok(!JSON.stringify(saved).includes(SECRET));
});

test("a part that failed its test is never saved", async () => {
  const api = new MockApiClient();
  const t: GuideTest = {
    mail: { state: "failed", report: { imap: "auth_failed", smtp: "auth_failed", ok: false } },
    calendar: { state: "ok", report: { ...goodCalendar, calendars: [goodCalendar.calendars[0]] } }
  };
  const saved = await savePlan(api, plan(), t, { mail: [], calendar: [] }, []);
  assert.equal(saved.mail, null);
  // With no calendar ticked, the tested address is what is saved.
  assert.equal(saved.calendars.length, 1);
  assert.ok(!(await api.listEmailAccounts()).some((a) => a.name === "icloud"));
});

test("a save the daemon refuses is reported, not swallowed", async () => {
  const api = {
    createEmailAccount: async () => {
      throw new Error("no");
    },
    createCalendarAccount: async () => {
      throw new Error("no");
    }
  };
  const t: GuideTest = { mail: { state: "ok", report: goodMail }, calendar: { state: "ok", report: goodCalendar } };
  const ticked = [{ name: "Home", url: "https://p01-caldav.icloud.com/123/calendars/home/" }];
  const saved = await savePlan(api, plan(), t, { mail: [], calendar: [] }, ticked);
  assert.equal(saved.mail, null);
  assert.deepEqual(saved.calendars, []);
  assert.deepEqual(saved.failed.map((f) => f.part), ["mail", "calendar"]);
});

test("what is connected lists the owner's mail and calendar, not a bot's own AgentMail address", async () => {
  const api = new MockApiClient();
  await api.createEmailAccount({ name: "bot-address", provider: "agentmail", api_key: "key-12345678", bot: ATLAS });
  const connected = await loadConnected(api);
  assert.ok(connected.mailboxes.length >= 1);
  assert.ok(!connected.mailboxes.some((a) => a.provider === "agentmail"));
});

test("disconnect everything removes every mailbox, calendar and Google mail/calendar sign-in", async () => {
  const api = new MockApiClient();
  await api.createCalendarAccount({ name: "home", url: "https://dav.example.com/home/", username: "me", password: "x-pass-1234" });
  const connected = await loadConnected(api);
  const googleCount = connected.googleApps.length;
  assert.ok(googleCount > 0, "the mock has a signed-in Google Calendar app");
  assert.ok(connected.mailboxes.length > 0 && connected.calendars.length > 0);
  const result = await disconnectAll(api, connected);
  assert.deepEqual(result.failed, []);
  assert.equal(result.removed, connected.mailboxes.length + connected.calendars.length + googleCount);
  const after = await loadConnected(api);
  assert.equal(after.mailboxes.length, 0);
  assert.equal(after.calendars.length, 0);
  assert.equal(after.googleApps.length, 0);
});

test("one that cannot be removed is named and does not stop the others", async () => {
  const api = new MockApiClient();
  const connected = await loadConnected(api);
  const removed: string[] = [];
  const result = await disconnectAll(
    {
      deleteEmailAccount: async (name) => {
        if (name === connected.mailboxes[0].name) throw new Error("locked");
        removed.push(name);
      },
      deleteCalendarAccount: async () => undefined,
      deleteMcpServer: async () => undefined
    },
    connected
  );
  assert.deepEqual(result.failed, [connected.mailboxes[0].name]);
  assert.equal(removed.length, connected.mailboxes.length - 1);
});

test("the Google check only counts a connected http server of the Gmail or Calendar app", () => {
  const server = (name: string, status: McpServer["status"], transport: McpServer["transport"] = "http") =>
    ({ name, status, transport }) as McpServer;
  assert.equal(googleSignedIn([server("google-gmail", "connected")], ["google-gmail", "google-calendar"]), true);
  assert.equal(googleSignedIn([server("google-gmail", "auth_required")], ["google-gmail"]), false);
  assert.equal(googleSignedIn([server("google-gmail", "connected", "stdio")], ["google-gmail"]), false);
  assert.equal(googleSignedIn([server("other", "connected")], ["google-gmail"]), false);
});

// ---- where the password may go -------------------------------------------------------------

const otherPlan = (over: Partial<GuidePlan> = {}): GuidePlan =>
  plan({
    provider: "other",
    address: "me@acme.example",
    mail: { on: true, servers: mailServers("other", "me@acme.example"), bot: ATLAS, allowSend: false },
    calendar: { on: true, server: "dav.acme.example" },
    ...over
  });

const recorder = () => {
  const seen: { mail: unknown[]; calendar: unknown[] } = { mail: [], calendar: [] };
  const api = {
    testEmailDraft: async (input: unknown) => {
      seen.mail.push(input);
      return goodMail;
    },
    testCalendar: async (input: unknown) => {
      seen.calendar.push(input);
      return goodCalendar;
    }
  };
  return { seen, api };
};

test("host names: only public-looking names, never an IP address in any spelling or a private-network name", () => {
  for (const ok of ["imap.acme.example", "mail.example.co.uk", "IMAP.Acme.Example.", "xn--mail-9ua.example.com"]) {
    assert.equal(isPublicHostName(ok), true, ok);
  }
  for (const bad of [
    "",
    "localhost",
    "imap",
    "127.0.0.1",
    "10.0.0.5",
    "192.168.1.20",
    "169.254.169.254",
    "0x7f.1",
    "2130706433",
    "[::1]",
    "::1",
    "nas.local",
    "mail.localhost",
    "mail.internal",
    "mail.home",
    "imap.acme.example:993",
    "imap.acme.example/x",
    "https://imap.acme.example",
    "a b.example.com",
    "-x.example.com",
    "a..example.com",
    `${"a".repeat(250)}.example.com`
  ]) {
    assert.equal(isPublicHostName(bad), false, bad);
  }
});

test("CalDAV addresses: https only, no login inside, no private place, nothing plain", () => {
  for (const ok of ["dav.acme.example", "https://dav.acme.example/cal/", "HTTPS://dav.acme.example", "dav.acme.example:8443/x"]) {
    assert.equal(calendarServerProblem(ok), null, ok);
    assert.notEqual(normaliseServerAddress(ok), "", ok);
  }
  assert.equal(calendarServerProblem(""), null);
  for (const text of ["http://dav.acme.example/", "ftp://dav.acme.example/", "webcal://dav.acme.example/", "file:///etc/passwd", "javascript://x"]) {
    assert.equal(calendarServerProblem(text), "scheme", text);
    assert.equal(normaliseServerAddress(text), "", text);
  }
  for (const text of ["https://127.0.0.1/", "https://localhost:5232/", "https://192.168.1.5/", "https://[::1]/", "https://nas.local/"]) {
    assert.equal(calendarServerProblem(text), "host", text);
    assert.equal(normaliseServerAddress(text), "", text);
  }
  assert.equal(calendarServerProblem("https://me:pw@dav.acme.example/"), "invalid");
  assert.equal(calendarServerProblem(`https://dav.acme.example/${"a".repeat(2100)}`), "invalid");
  // What reaches the daemon for an address that is not allowed is empty, never the typed text.
  const bad = otherPlan({ calendar: { on: true, server: "http://dav.acme.example/" } });
  assert.equal(calendarUrl(bad), "");
});

test("mail ports: IMAP 993, SMTP 465 with TLS or 587 with STARTTLS, nothing else, never plain", () => {
  assert.deepEqual(planProblems(otherPlan()), []);
  assert.deepEqual(planProblems(plan()), []);
  for (const provider of ["google", "icloud", "yahoo", "fastmail", "microsoft"] as const) {
    const known = plan({ provider, mail: { on: true, servers: mailServers(provider, "me@x.com"), bot: ATLAS, allowSend: false } });
    assert.deepEqual(planProblems({ ...known, calendar: null }), [], provider);
  }
  const servers = (over: Partial<ReturnType<typeof mailServers>>) => ({
    on: true,
    servers: { ...mailServers("other", "me@acme.example"), ...over },
    bot: ATLAS,
    allowSend: false
  });
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ imapPort: 143 }) })), ["imapPort"]);
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ smtpPort: 25 }) })), ["smtpPort"]);
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ smtpPort: 587 }) })), ["smtpPort"], "587 is STARTTLS only");
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ smtpPort: 587, smtpSecurity: "start_tls" }) })), []);
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ smtpPort: 465, smtpSecurity: "start_tls" }) })), ["smtpPort"]);
  assert.deepEqual(planProblems(otherPlan({ mail: servers({ imapHost: "127.0.0.1", smtpHost: "localhost" }) })), ["imapHost", "smtpHost"]);
  assert.deepEqual(
    planProblems(otherPlan({ calendar: { on: true, server: "http://dav.acme.example" } })),
    ["calendarScheme"]
  );
});

test("only the known providers go straight through; a guess or an edit needs the owner's approval", () => {
  for (const provider of ["google", "icloud", "yahoo", "fastmail"] as const) {
    const known = plan({ provider, mail: { on: true, servers: mailServers(provider, "me@x.com"), bot: ATLAS, allowSend: false } });
    assert.equal(needsApproval(known), false, provider);
    assert.equal(isApproved(known), true, provider);
  }
  assert.equal(needsApproval(otherPlan()), true);
  assert.equal(isApproved(otherPlan()), false);
  const edited = plan({
    mail: { on: true, servers: { ...mailServers("icloud", "me@icloud.com"), imapHost: "imap.evil.example" }, bot: ATLAS, allowSend: false }
  });
  assert.equal(serversEdited(edited), true);
  assert.equal(needsApproval(edited), true);
  assert.equal(isApproved(edited), false);
});

test("nothing is sent to a guessed or edited server until it is approved, and an edit takes the approval back", async () => {
  const { seen, api } = recorder();
  await assert.rejects(testPlan(api, otherPlan()), /not approved/);
  assert.equal(seen.mail.length + seen.calendar.length, 0, "no request, so no password, left");

  const approved = otherPlan();
  approved.approved = targetKey(approved);
  const result = await testPlan(api, approved);
  assert.ok(allPassed(result));
  assert.equal(seen.mail.length, 1);
  assert.equal(seen.calendar.length, 1);

  // Approved for one set of servers, then the owner changes one: not approved any more.
  const moved = otherPlan({
    mail: { on: true, servers: { ...mailServers("other", "me@acme.example"), imapHost: "imap.evil.example" }, bot: ATLAS, allowSend: false }
  });
  moved.approved = approved.approved;
  assert.equal(isApproved(moved), false);
  await assert.rejects(testPlan(api, moved), /not approved/);
  const calendarMoved = otherPlan({ calendar: { on: true, server: "dav.evil.example" } });
  calendarMoved.approved = approved.approved;
  await assert.rejects(testPlan(api, calendarMoved), /not approved/);
  assert.equal(seen.mail.length, 1);
  assert.equal(seen.calendar.length, 1);
});

test("a plan with unusable servers or an address that is not https is refused before anything is sent", async () => {
  const { seen, api } = recorder();
  for (const bad of [
    otherPlan({ calendar: { on: true, server: "http://dav.acme.example/" } }),
    otherPlan({ mail: { on: true, servers: { ...mailServers("other", "me@acme.example"), imapPort: 143 }, bot: ATLAS, allowSend: false } }),
    otherPlan({ mail: { on: true, servers: { ...mailServers("other", "me@acme.example"), smtpHost: "10.0.0.1" }, bot: ATLAS, allowSend: false } })
  ]) {
    bad.approved = targetKey(bad);
    await assert.rejects(testPlan(api, bad), /not usable/);
  }
  assert.equal(seen.mail.length + seen.calendar.length, 0);
});

test("the tests ask the daemon for public servers only; the saved account carries no such flag", async () => {
  const { seen, api } = recorder();
  await testPlan(api, plan());
  assert.equal((seen.mail[0] as { public_hosts_only?: boolean }).public_hosts_only, true);
  assert.equal((seen.calendar[0] as { public_only?: boolean }).public_only, true);
  assert.equal("public_hosts_only" in mailInput(plan(), "icloud"), false);
});

test("raw failure text maps to a fix by cause only, and every fix exists in English and Turkish", () => {
  assert.equal(calendarProblem("The address must start with https://. A password is never sent"), "insecure");
  assert.equal(calendarProblem("This address is not allowed: it points at this computer"), "refused");
  assert.equal(errorFixKey({ status: 422, code: "bad_email_config", message: "https://evil.example hunter2" }), "error.rejected");
  assert.equal(errorFixKey({ status: 429 }), "error.busy");
  assert.equal(errorFixKey({ status: 0, code: "network" }), "error.network");
  assert.equal(errorFixKey({ status: 403 }), "error.denied");
  assert.equal(errorFixKey({ status: 409 }), "error.conflict");
  assert.equal(errorFixKey(new Error("https://evil.example?token=abc")), "error.generic");
  assert.equal(errorFixKey(null), "error.generic");
  // calendarFixKey never gives the text back: an unknown failure is the generic cause.
  assert.equal(calendarFixKey("other", "weird https://evil.example failure"), "calendar.other");

  const keys = [
    ...["network", "busy", "denied", "rejected", "conflict", "generic"].map((k) => `fix.error.${k}`),
    "fix.calendar.insecure",
    "fix.calendar.refused",
    ...["imapHost", "smtpHost", "imapPort", "smtpPort", "calendarScheme", "calendarHost", "calendarInvalid"].map((k) => `problems.${k}`),
    ...["title", "guessed", "edited", "imap", "smtp", "caldav", "caldavNone", "confirm"].map((k) => `approve.${k}`),
    "missing.problem",
    "missing.approve",
    "more.smtpSecurity",
    "more.security.implicit",
    "more.security.start_tls",
    "result.error"
  ];
  for (const key of keys) {
    for (const [lng, tree] of [["en", read("en")], ["tr", read("tr")]] as const) {
      const text = lookup(tree, `settingsModal.connectMail.${key}`);
      assert.equal(typeof text, "string", `${lng} is missing ${key}`);
    }
  }
});

test("a long list of found calendars is cut to 20, https only, with short names", async () => {
  const many = Array.from({ length: 300 }, (_, i) => ({ name: `Cal ${i} ${"n".repeat(200)}`, url: `https://dav.example.com/${i}/` }));
  const report: CalendarTestReport = {
    ok: true,
    scheduling: false,
    error: null,
    calendars: [
      { name: "Plain", url: "http://dav.example.com/plain/" },
      { name: "Huge", url: `https://dav.example.com/${"a".repeat(3000)}` },
      ...many
    ]
  };
  const cut = foundCalendars(report);
  assert.equal(cut.length, MAX_CALENDARS);
  assert.ok(cut.every((c) => c.url.startsWith("https://") && c.url.length <= 2048 && c.name.length <= 80));
  assert.ok(!cut.some((c) => c.name === "Plain" || c.name === "Huge"));

  // Saving never goes past the cap, even when the caller hands over more.
  const created: string[] = [];
  const api = {
    createEmailAccount: async () => ({}) as never,
    createCalendarAccount: async (input: { name?: string }) => {
      created.push(input.name ?? "");
      return {} as never;
    }
  };
  const test_: GuideTest = { mail: null, calendar: { state: "ok", report } };
  const saved = await savePlan(api, plan({ mail: null }), test_, { mail: [], calendar: [] }, [
    { name: "Plain", url: "http://dav.example.com/plain/" },
    ...many.map((c) => ({ name: c.name, url: c.url }))
  ]);
  assert.equal(saved.calendars.length, MAX_CALENDARS);
  assert.equal(created.length, MAX_CALENDARS);
});
