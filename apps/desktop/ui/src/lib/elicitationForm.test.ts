import assert from "node:assert/strict";
import { test } from "node:test";
import type { ElicitationField } from "../api/types.ts";
import { buildContent, defaultChecked, defaultText, hasUnsatisfiableField, inputType, isElicitationGone } from "./elicitationForm.ts";

const f = (over: Partial<ElicitationField> & Pick<ElicitationField, "name" | "type">): ElicitationField => ({
  title: null,
  description: null,
  required: false,
  ...over
});

const email = f({ name: "email", type: "string", required: true, format: "email" });
const seats = f({ name: "seats", type: "integer", minimum: 1, maximum: 9 });
const note = f({ name: "note", type: "string", min_length: 2, max_length: 5 });
const news = f({ name: "news", type: "boolean" });

test("builds content: empty optionals left out, integers as numbers, booleans always sent", () => {
  assert.deepEqual(buildContent([email, seats, note, news], ["a@b.c", "", "  ", false]), {
    ok: true,
    content: { email: "a@b.c", news: false }
  });
  assert.deepEqual(buildContent([email, seats, news], ["a@b.c", " 3 ", true]), {
    ok: true,
    content: { email: "a@b.c", seats: 3, news: true }
  });
});

test("names the first field that does not fit", () => {
  const problem = (fields: ElicitationField[], raw: (string | boolean)[]) => {
    const r = buildContent(fields, raw);
    return r.ok ? null : [r.field.name, r.problem];
  };
  assert.deepEqual(problem([email], [" "]), ["email", "required"]);
  assert.deepEqual(problem([seats], ["abc"]), ["seats", "number"]);
  assert.deepEqual(problem([seats], ["2.5"]), ["seats", "integer"]);
  assert.deepEqual(problem([seats], ["0"]), ["seats", "tooSmall"]);
  assert.deepEqual(problem([seats], ["10"]), ["seats", "tooLarge"]);
  assert.deepEqual(problem([note], ["a"]), ["note", "tooShort"]);
  assert.deepEqual(problem([note], ["abcdef"]), ["note", "tooLong"]);
  // characters, not UTF-16 units: five emoji fit max_length 5
  assert.equal(problem([note], ["😀😀😀😀😀"]), null);
});

test("a local date-time is sent as RFC 3339", () => {
  const when = f({ name: "when", type: "string", format: "date-time" });
  const r = buildContent([when], ["2026-11-26T10:30"]);
  assert.ok(r.ok);
  assert.match(String(r.content.when), /^2026-11-2\dT\d\d:30:00\.000Z$/);
});

test("input types follow the format, and only a fitting schema default is prefilled", () => {
  assert.equal(inputType(email), "email");
  assert.equal(inputType(f({ name: "u", type: "string", format: "uri" })), "url");
  assert.equal(inputType(f({ name: "d", type: "string", format: "date" })), "date");
  assert.equal(inputType(f({ name: "t", type: "string", format: "date-time" })), "datetime-local");
  assert.equal(inputType(seats), "number");
  assert.equal(defaultText({ ...seats, default: 3 }), "3");
  assert.equal(defaultText({ ...seats, default: "3" }), undefined);
  assert.equal(defaultText(email), undefined);
  assert.equal(defaultChecked({ ...news, default: true }), true);
  assert.equal(defaultChecked(news), false);
});

test("knows the code that closes the card", () => {
  assert.equal(isElicitationGone("elicitation_gone"), true);
  assert.equal(isElicitationGone("secret_request_gone"), false);
});

test("a blocked field never puts its value in the content, whatever the raw value", () => {
  const apiKey = f({ name: "api_key", type: "string", blocked: true });
  const flag = f({ name: "remember", type: "boolean", blocked: true });
  const num = f({ name: "pin", type: "integer", blocked: true });
  assert.deepEqual(buildContent([email, apiKey, flag, num, news], ["a@b.c", "sk-live-123", true, "1234", true]), {
    ok: true,
    content: { email: "a@b.c", news: true }
  });
  const built = buildContent([apiKey], ["sk-live-123"]);
  assert.equal(built.ok && JSON.stringify(built.content).includes("sk-live"), false);
  // Raw values past the end are not read either.
  assert.deepEqual(buildContent([apiKey], []), { ok: true, content: {} });
});

test("a required blocked field can never be satisfied", () => {
  const code = f({ name: "otp", type: "string", required: true, blocked: true });
  const r = buildContent([email, code], ["a@b.c", "123456"]);
  assert.equal(r.ok, false);
  assert.deepEqual(r.ok ? null : [r.field.name, r.problem], ["otp", "blocked"]);
  assert.equal(buildContent([code], [undefined]).ok, false);
});

test("hasUnsatisfiableField is true only for a required blocked field", () => {
  const optional = f({ name: "token", type: "string", blocked: true });
  const required = f({ name: "password", type: "string", required: true, blocked: true });
  const notBlocked = f({ name: "x", type: "string", required: true, blocked: false });
  assert.equal(hasUnsatisfiableField([]), false);
  assert.equal(hasUnsatisfiableField([email, optional, notBlocked]), false);
  assert.equal(hasUnsatisfiableField([email, required]), true);
});
