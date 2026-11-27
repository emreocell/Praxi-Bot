import assert from "node:assert/strict";
import test from "node:test";
import { canViewInWindow, MAX_CITED_LINE, MAX_CITED_PAGE, openCitation, parseCitationHref, safeFileName, viewerUrl } from "./documentCitation.ts";

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
    // an encoded slash makes one segment hold a path
    `praxi-doc://${ID}/a%2Fb.txt`,
    `praxi-doc://${ID}/a%2fb.txt`,
    `praxi-doc://${ID}/%2E%2E%2Fsecret.txt`,
    `praxi-doc://${ID}/%2e%2e%2f%2e%2e%2fetc%2fpasswd`,
    `praxi-doc://${ID}/a/%2E%2E%2Fb.txt`,
    `praxi-doc://${ID}/%2F`,
    // dot segments, encoded or not
    `praxi-doc://${ID}/%2E/a.txt`,
    `praxi-doc://${ID}/a/./b.txt`,
    `praxi-doc://${ID}/a//b.txt`,
    `praxi-doc://${ID}/a/%2e%2e`,
    // backslash, drive and stream forms, trailing dot or space
    `praxi-doc://${ID}/a/b%5Cc.txt`,
    `praxi-doc://${ID}/C%3A/a.txt`,
    `praxi-doc://${ID}/a.txt%3Astream`,
    `praxi-doc://${ID}/a.txt.`,
    `praxi-doc://${ID}/a.txt%20`,
    // control characters
    `praxi-doc://${ID}/a%0Ab.txt`,
    `praxi-doc://${ID}/a%0Db.txt`,
    `praxi-doc://${ID}/a%7Fb.txt`,
    `praxi-doc://${ID}/a%09b.txt`,
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

test("page and line numbers are clamped, not trusted", () => {
  const c = parseCitationHref(`praxi-doc://${ID}/a.pdf?page=999999999&lines=5-999999999`);
  assert.equal(c?.page, MAX_CITED_PAGE);
  assert.deepEqual(c?.lines, { from: 5, to: MAX_CITED_LINE });
  assert.equal(parseCitationHref(`praxi-doc://${ID}/a.pdf?page=12345678901`)?.page, null);
  assert.equal(viewerUrl("blob:x", 999_999_999), `blob:x#page=${MAX_CITED_PAGE}`);
  assert.equal(viewerUrl("blob:x", 0), "blob:x");
  assert.equal(viewerUrl("blob:x", 1.5), "blob:x");
  assert.equal(viewerUrl("blob:x", Number.NaN), "blob:x");
});

test("a saved copy gets a clean one-segment name", () => {
  assert.equal(safeFileName("2024 return.pdf"), "2024 return.pdf");
  assert.equal(safeFileName("..\\..\\evil.exe"), "evil.exe");
  assert.equal(safeFileName("../../evil.exe"), "evil.exe");
  assert.equal(safeFileName("a/b:c*d?.txt"), "b_c_d_.txt");
  assert.equal(safeFileName("bad\u0000\r\nname.txt"), "bad___name.txt");
  assert.equal(safeFileName(".."), "document");
  assert.equal(safeFileName("   "), "document");
  assert.equal(safeFileName("x".repeat(500)).length, 120);
});

test("opening a citation saves under a cleaned name even if the citation object was built by hand", async () => {
  const saved: string[] = [];
  const client = { getKnowledgeFile: async () => new Blob(["x"]) };
  const URLWithBlob = URL as unknown as { createObjectURL: (b: Blob) => string };
  const original = URLWithBlob.createObjectURL;
  URLWithBlob.createObjectURL = () => "blob:test";
  try {
    const result = await openCitation(
      client,
      "bot",
      { sourceId: ID, locator: "a/b.txt", fileName: "../../evil\u0000.txt", page: null, lines: null },
      { open: () => false, save: (_url, name) => saved.push(name), revokeLater: () => {} }
    );
    assert.equal(result, "saved");
    assert.deepEqual(saved, ["evil_.txt"]);
  } finally {
    URLWithBlob.createObjectURL = original;
  }
});
