import assert from "node:assert/strict";
import { test } from "node:test";
import {
  parseComputerTarget,
  parseSendWith,
  shouldSend
} from "./settingsPrefs.ts";

const key = (over: Partial<Parameters<typeof shouldSend>[0]> = {}) => ({
  key: "Enter",
  shiftKey: false,
  ctrlKey: false,
  metaKey: false,
  ...over
});

test("send with Enter: Shift+Enter and IME composition do not send", () => {
  assert.equal(shouldSend(key(), "enter"), true);
  assert.equal(shouldSend(key({ shiftKey: true }), "enter"), false);
  assert.equal(shouldSend(key({ isComposing: true }), "enter"), false);
  assert.equal(shouldSend(key({ key: "a" }), "enter"), false);
});

test("send with Ctrl+Enter: plain Enter breaks the line", () => {
  assert.equal(shouldSend(key(), "ctrl-enter"), false);
  assert.equal(shouldSend(key({ ctrlKey: true }), "ctrl-enter"), true);
  assert.equal(shouldSend(key({ metaKey: true }), "ctrl-enter"), true);
});

test("stored preferences fall back to safe defaults", () => {
  assert.equal(parseSendWith("nope"), "enter");
  assert.equal(parseSendWith("ctrl-enter"), "ctrl-enter");
  assert.equal(parseComputerTarget("remote"), "remote");
  assert.equal(parseComputerTarget(undefined), "this-pc");
});
