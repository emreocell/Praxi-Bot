import assert from "node:assert/strict";
import { test } from "node:test";
import { escapeInvisible } from "./approvalPlain.ts";
import {
  buildSubmitBody,
  formatCountdown,
  parseAllowList,
  secondsLeft
} from "./secretRequestForm.ts";

test("a login needs username and password, a secret needs its value, and the shapes never mix", () => {
  assert.equal(buildSubmitBody("login", { username: "u" }, [], []), null);
  assert.equal(buildSubmitBody("secret", { value: "" }, [], []), null);
  assert.deepEqual(buildSubmitBody("login", { username: "u", password: "p", value: "ignored" }, ["https://a.example"], []), {
    username: "u",
    password: "p",
    origins: ["https://a.example"],
    programs: []
  });
  assert.deepEqual(buildSubmitBody("secret", { value: "v", username: "ignored" }, [], []), {
    value: "v",
    origins: [],
    programs: []
  });
});

test("allow-lists split on lines, commas and spaces without repeats", () => {
  assert.deepEqual(parseAllowList("https://a.example,\nhttps://b.example  https://a.example\n"), [
    "https://a.example",
    "https://b.example"
  ]);
  assert.deepEqual(parseAllowList("  "), []);
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
