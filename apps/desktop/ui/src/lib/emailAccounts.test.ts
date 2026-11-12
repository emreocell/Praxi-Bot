import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import type { EmailAccount } from "../api/types.ts";
import {
  applyPreset,
  botFromScope,
  emptyForm,
  EMAIL_PRESETS,
  formFromAccount,
  needsPassword,
  presetOf,
  probeTone,
  scopeFromBot,
  toInput,
  toPatch,
  validateForm
} from "./emailAccounts.ts";

const ATLAS = "11111111-1111-4111-8111-111111111111";

function account(over: Partial<EmailAccount> = {}): EmailAccount {
  return {
    name: "work",
    address: "me@gmail.com",
    username: "",
    imap: { host: "imap.gmail.com", port: 993 },
    smtp: { host: "smtp.gmail.com", port: 465, security: "implicit" },
    drafts_folder: "Drafts",
    bot: null,
    allowed_senders: [],
    password_set: true,
    custom_ca: false,
    ...over
  };
}

function filled() {
  return {
    ...applyPreset(emptyForm(), "gmail"),
    name: "work",
    address: "me@gmail.com",
    password: "app-password"
  };
}

test("presets prefill the servers and custom keeps what was typed", () => {
  const gmail = applyPreset(emptyForm(), "gmail");
  assert.equal(gmail.imapHost, "imap.gmail.com");
  assert.equal(gmail.imapPort, "993");
  assert.equal(gmail.smtpHost, "smtp.gmail.com");
  assert.equal(gmail.smtpPort, "465");
  assert.equal(gmail.smtpSecurity, "implicit");
  const icloud = applyPreset(emptyForm(), "icloud");
  assert.equal(icloud.smtpSecurity, "start_tls");
  assert.equal(icloud.smtpPort, "587");
  for (const id of ["gmail", "outlook", "yahoo", "icloud"] as const) {
    const f = applyPreset(emptyForm(), id);
    assert.equal(f.preset, id);
    assert.ok(f.imapHost && f.smtpHost, id);
  }
  // Switching to custom keeps the hosts so they can be edited.
  const custom = applyPreset(gmail, "custom");
  assert.equal(custom.preset, "custom");
  assert.equal(custom.imapHost, "imap.gmail.com");
  // Every preset is a distinct, valid server pair.
  assert.equal(new Set(EMAIL_PRESETS.map((p) => p.id)).size, EMAIL_PRESETS.length);
});

test("a stored account maps back to its preset or custom", () => {
  assert.equal(presetOf(account()), "gmail");
  assert.equal(
    presetOf(account({ imap: { host: "mail.example.org", port: 993 } })),
    "custom"
  );
});

test("a valid form has no errors and builds the request body", () => {
  const form = filled();
  assert.deepEqual(validateForm(form, { existing: [], editing: false }), []);
  const body = toInput(form);
  assert.equal(body.name, "work");
  assert.equal(body.password, "app-password");
  assert.deepEqual(body.imap, { host: "imap.gmail.com", port: 993 });
  assert.deepEqual(body.smtp, {
    host: "smtp.gmail.com",
    port: 465,
    security: "implicit"
  });
  // Every bot: no binding is sent.
  assert.equal(body.bot, null);
  assert.deepEqual(body.allowed_senders, []);
});

test("validation mirrors the daemon's rules", () => {
  const bad = (patch: object) =>
    validateForm({ ...filled(), ...patch }, { existing: ["taken"], editing: false });
  assert.deepEqual(bad({ name: "" }), ["name"]);
  assert.deepEqual(bad({ name: "a b" }), ["name"]);
  assert.deepEqual(bad({ name: "taken" }), ["name"]);
  assert.deepEqual(bad({ address: "not an address" }), ["address"]);
  assert.deepEqual(bad({ imapHost: "imap.example.com/x" }), ["imapHost"]);
  assert.deepEqual(bad({ imapHost: "imap.example.com\nA1" }), ["imapHost"]);
  assert.deepEqual(bad({ smtpHost: "" }), ["smtpHost"]);
  assert.deepEqual(bad({ imapPort: "0" }), ["imapPort"]);
  assert.deepEqual(bad({ imapPort: "70000" }), ["imapPort"]);
  assert.deepEqual(bad({ smtpPort: "25x" }), ["smtpPort"]);
  assert.deepEqual(bad({ password: "" }), ["password"]);
  assert.deepEqual(bad({ allowedSenders: "boss@example.com\nnope" }), ["senders"]);
  assert.deepEqual(bad({ allowedSenders: "boss@example.com, @corp.example" }), []);
  // "Only this bot" with no bot picked is an error, never "every bot".
  assert.deepEqual(bad({ bot: { mode: "some", ids: [] } }), ["bot"]);
  assert.deepEqual(bad({ bot: { mode: "some", ids: [ATLAS] } }), []);
  // Editing may leave the password empty (the stored one stays).
  const edit = validateForm(
    { ...filled(), password: "" },
    { existing: [], editing: true }
  );
  assert.deepEqual(edit, []);
});

test("the bot choice is a single bot or everyone, and never widens by accident", () => {
  assert.deepEqual(scopeFromBot(null), { mode: "all" });
  assert.deepEqual(scopeFromBot(ATLAS), { mode: "some", ids: [ATLAS] });
  assert.equal(botFromScope({ mode: "all" }), null);
  assert.equal(botFromScope({ mode: "some", ids: [ATLAS] }), ATLAS);
  // An emptied "only this bot" is not "all bots": it stays unresolved.
  assert.equal(botFromScope({ mode: "some", ids: [] }), undefined);
  const form = { ...filled(), bot: { mode: "some" as const, ids: [ATLAS] } };
  assert.equal(toInput(form).bot, ATLAS);
});

test("an edit sends only what changed; unbinding is an explicit null", () => {
  const original = account({ bot: ATLAS, allowed_senders: ["a@example.com"] });
  const same = formFromAccount(original);
  assert.equal(same.password, "");
  assert.deepEqual(toPatch(same, original), {});

  assert.deepEqual(toPatch({ ...same, bot: { mode: "all" } }, original), {
    bot: null
  });
  assert.deepEqual(
    toPatch({ ...same, allowedSenders: "a@example.com\nb@example.com" }, original),
    { allowed_senders: ["a@example.com", "b@example.com"] }
  );
  assert.deepEqual(toPatch({ ...same, password: "new-pw" }, original), {
    password: "new-pw"
  });
  const moved = toPatch({ ...same, imapHost: "imap.other.net", password: "pw" }, original);
  assert.deepEqual(moved.imap, { host: "imap.other.net", port: 993 });
  assert.equal(moved.password, "pw");
  assert.ok(!("smtp" in moved));
});

test("changing servers or login needs the password again", () => {
  const original = account();
  const same = formFromAccount(original);
  assert.equal(needsPassword(same, original), false);
  assert.equal(needsPassword({ ...same, imapHost: "x.example.net" }, original), true);
  assert.equal(needsPassword({ ...same, smtpPort: "587" }, original), true);
  assert.equal(needsPassword({ ...same, username: "other" }, original), true);
  assert.equal(needsPassword({ ...same, address: "else@gmail.com" }, original), true);
  assert.equal(
    needsPassword({ ...same, allowedSenders: "x@example.com" }, original),
    false
  );
  // Editing with a changed server and no password is an error on the field.
  const errors = validateForm(
    { ...same, imapHost: "x.example.net" },
    { existing: [], editing: true, original }
  );
  assert.deepEqual(errors, ["password"]);
});

test("probe statuses map to a tone", () => {
  assert.equal(probeTone("ok"), "ok");
  for (const s of ["unreachable", "tls", "auth_failed", "protocol", "timeout"] as const) {
    assert.equal(probeTone(s), "bad", s);
  }
});

test("the mock daemon runs the same client paths and never returns the password", async () => {
  const c = new MockApiClient();
  const before = await c.listEmailAccounts();
  assert.ok(before.length >= 1);
  const created = await c.createEmailAccount(toInput({ ...filled(), name: "new" }));
  assert.equal(created.name, "new");
  assert.equal(created.password_set, true);
  assert.ok(!JSON.stringify(await c.listEmailAccounts()).includes("app-password"));
  const patched = await c.updateEmailAccount("new", { bot: ATLAS });
  assert.equal(patched.bot, ATLAS);
  const unbound = await c.updateEmailAccount("new", { bot: null });
  assert.equal(unbound.bot, null);
  const report = await c.testEmailAccount("new");
  assert.equal(report.ok, true);
  const draft = await c.testEmailDraft(toInput(filled()));
  assert.equal(draft.imap, "ok");
  await c.deleteEmailAccount("new");
  assert.equal((await c.listEmailAccounts()).length, before.length);
  await assert.rejects(() => c.deleteEmailAccount("new"));
});
