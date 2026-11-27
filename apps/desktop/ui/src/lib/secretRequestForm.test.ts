import assert from "node:assert/strict";
import { test } from "node:test";
import { escapeInvisible } from "./approvalPlain.ts";
import {
  buildSubmitBody,
  formatCountdown,
  secondsLeft
} from "./secretRequestForm.ts";

test("a login needs username and password, a secret needs its value, and the shapes never mix", () => {
  assert.equal(buildSubmitBody("login", { username: "user01" }), null);
  assert.equal(buildSubmitBody("secret", { value: "" }), null);
  assert.equal(buildSubmitBody("login", { username: "short", password: "passwd" }), null);
  assert.deepEqual(buildSubmitBody("login", { username: "user01", password: "passwd", value: "ignored" }), {
    username: "user01",
    password: "passwd"
  });
  assert.deepEqual(buildSubmitBody("secret", { value: "single", username: "ignored" }), {
    value: "single"
  });
});

test("the countdown never goes negative and reads m:ss", () => {
  assert.equal(secondsLeft(10_000, 4_500), 6);
  assert.equal(secondsLeft(10_000, 20_000), 0);
  assert.equal(formatCountdown(245), "4:05");
});

test("purpose text from the bot has bidi and zero-width characters written out", () => {
  const shown = escapeInvisible("Sign in‮gnp.exe​");
  assert.ok(![...shown].some((ch) => ["‮", "​"].includes(ch)));
  assert.ok(shown.includes("<U+202E>") && shown.includes("<U+200B>"));
});
