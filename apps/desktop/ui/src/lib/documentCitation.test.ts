import assert from "node:assert/strict";
import test from "node:test";
import { canViewInWindow, parseCitationHref, viewerUrl } from "./documentCitation.ts";

const ID = "0190f3c2-7d9a-7a11-8c01-4f2f3a6b9c10";

test("a citation link names the source, file, page and lines", () => {
  const c = parseCitationHref(`praxi-doc://${ID}/taxes/2024%20return.pdf?lines=12-19&page=3`);
  assert.deepEqual(c, {
    sourceId: ID,
    locator: "taxes/2024 return.pdf",
    fileName: "2024 return.pdf",
    page: 3,
    lines: { from: 12, to: 19 }
  });
});

test("page and lines are optional", () => {
  const c = parseCitationHref(`praxi-doc://${ID}/notes.md`);
  assert.equal(c?.page, null);
  assert.equal(c?.lines, null);
  assert.equal(c?.fileName, "notes.md");
});

test("anything that is not a well-formed citation link is refused", () => {
  for (const bad of [
    "https://example.com/a.pdf",
    "javascript:alert(1)",
    `praxi-doc://not-a-uuid/a.pdf`,
    `praxi-doc://${ID}`,
    `praxi-doc://${ID}/`,
    `praxi-doc://${ID}/../secret.txt`,
    `praxi-doc://${ID}/a/%2E%2E/b.txt`,
    `praxi-doc://${ID}/a%5Cb.txt`,
    `praxi-doc://${ID}/a%00b.txt`,
    `praxi-doc://${ID}/a%ZZ.txt`,
    `praxi-doc://user:pw@${ID}/a.txt`,
    `praxi-doc://${ID}/a.txt#frag`
  ]) {
    assert.equal(parseCitationHref(bad), null, bad);
  }
});

test("a nonsense page or line range is dropped, not trusted", () => {
  const c = parseCitationHref(`praxi-doc://${ID}/a.pdf?page=0&lines=9-3`);
  assert.equal(c?.page, null);
  assert.equal(c?.lines, null);
  assert.equal(parseCitationHref(`praxi-doc://${ID}/a.pdf?page=abc`)?.page, null);
});

test("only a PDF is shown in a window, opened on its page", () => {
  assert.equal(canViewInWindow("Lease.PDF"), true);
  assert.equal(canViewInWindow("budget.xlsx"), false);
  assert.equal(viewerUrl("blob:x", 4), "blob:x#page=4");
  assert.equal(viewerUrl("blob:x", null), "blob:x");
});
