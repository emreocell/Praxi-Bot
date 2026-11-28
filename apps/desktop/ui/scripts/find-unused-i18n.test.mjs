import assert from "node:assert/strict";
import { test } from "node:test";
import { findUnusedKeys } from "./find-unused-i18n.mjs";

test("daemon error codes are never listed as unused", () => {
  const { unused } = findUnusedKeys();
  assert.ok(!unused.includes("errors.codes.network.title"));
  assert.ok(!unused.includes("errors.codes.bot_limit_reached.hint"));
});

test("menu tables and ternaries keep account keys", () => {
  const { unused } = findUnusedKeys();
  assert.ok(!unused.includes("account.ai"));
  assert.ok(!unused.includes("widget.stopPending"));
});

test("mcp runtime hints stay", () => {
  const { unused } = findUnusedKeys();
  assert.ok(!unused.includes("apps.error.hint.runtime_uv"));
});