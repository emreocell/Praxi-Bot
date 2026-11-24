import assert from "node:assert/strict";
import { test } from "node:test";
import { safeFileName } from "./attachmentActions.ts";
import { requestSearchAllChats, searchPhrase, takeSearchRequest } from "./chatSearchRequest.ts";

test("a saved file name cannot carry a path or control characters", () => {
  assert.ok(!/[\\/]/.test(safeFileName("..\\..\\Windows\\evil.exe")));
  assert.ok(!/[\\/]/.test(safeFileName("../../etc/passwd")));
  assert.ok(!safeFileName(".hidden").startsWith("."));
  assert.equal(safeFileName("  "), "file");
  assert.equal(safeFileName("report\u0000.pdf"), "report_.pdf");
});

test("search-all request is one line of at most 200 characters and is taken once", () => {
  assert.equal(searchPhrase("  a\n  b\tc "), "a b c");
  assert.equal(searchPhrase("x".repeat(500)).length, 200);
  // no window in node: nothing is queued, nothing throws
  requestSearchAllChats("hello");
  assert.equal(takeSearchRequest(), null);
});
