import assert from "node:assert/strict";
import test from "node:test";
import { estimateParts, finishIndexing, folderName, overlappingFolder, typeCounts } from "./documents.ts";
import type { KnowledgeMutation, KnowledgeSource } from "../api/types";

const source = (locator: string, revision = 1, kind: KnowledgeSource["kind"] = "folder"): KnowledgeSource => ({
  id: "s1", kind, label: "Docs", locator, enabled: true, requires_path_selection: false, revision,
  remote_profile_fingerprint: null, documents: 0, chunks: 0, updated_ms: 0
});
const empty = { listed: [], secret: 0, too_large: 0, hidden: 0, unsupported: 0 };
const page = (processed: number, revision: number, next: string | null, failures: { locator: string; code: string }[] = []): KnowledgeMutation => ({
  source: source("/d", revision),
  indexing: { processed, unchanged: 0, removed: 0, next_cursor: next, failures, skipped: empty }
});

test("estimates are said in minutes or hours, never in raw seconds", () => {
  assert.deepEqual(estimateParts(3), { unit: "lessThanMinute" });
  assert.deepEqual(estimateParts(44), { unit: "lessThanMinute" });
  assert.deepEqual(estimateParts(50), { unit: "minutes", value: 1 });
  assert.deepEqual(estimateParts(12 * 60), { unit: "minutes", value: 12 });
  assert.deepEqual(estimateParts(3 * 3600), { unit: "hours", value: 3 });
  assert.deepEqual(estimateParts(Number.NaN), { unit: "lessThanMinute" });
});

test("a folder that is, contains or sits inside an added folder is recognised", () => {
  const added = [source("/home/ada/Documents"), source("/home/ada/Notes.md", 1, "file")];
  assert.ok(overlappingFolder(added, "/home/ada/Documents/"));
  assert.ok(overlappingFolder(added, "/home/ada/Documents/Taxes"));
  assert.ok(overlappingFolder(added, "/home/ada"));
  assert.equal(overlappingFolder(added, "/home/ada/Documents2"), undefined);
  assert.equal(overlappingFolder(added, "/home/bob"), undefined);
  assert.ok(overlappingFolder([source("C:\\Users\\Ada\\Docs")], "c:\\users\\ada\\docs\\taxes"));
});

test("names and type counts read naturally", () => {
  assert.equal(folderName("/home/ada/Documents/"), "Documents");
  assert.equal(folderName("C:\\Users\\Ada\\Taxes"), "Taxes");
  assert.deepEqual(typeCounts({ code: 1, xlsx: 2, pdf: 5, text: 0 }), [
    { type: "pdf", count: 5 },
    { type: "xlsx", count: 2 },
    { type: "code", count: 1 }
  ]);
});

test("indexing continues page by page with the newest revision and reports progress", async () => {
  const calls: { revision: number; cursor?: string }[] = [];
  const pages = [page(10, 3, "b.md"), page(10, 5, "c.md", [{ locator: "x.pdf", code: "secret_content" }]), page(4, 6, null)];
  const client = {
    reindexKnowledgeSource: async (_bot: string, _id: string, revision: number, cursor?: string) => {
      calls.push({ revision, cursor });
      return pages[calls.length];
    }
  };
  const seen: number[] = [];
  const done = await finishIndexing(client, "bot", pages[0], (p) => seen.push(p.indexed));
  assert.deepEqual(seen, [10, 20, 24]);
  assert.deepEqual(calls, [{ revision: 3, cursor: "b.md" }, { revision: 5, cursor: "c.md" }]);
  assert.equal(done.indexed, 24);
  assert.deepEqual(done.failures, [{ locator: "x.pdf", code: "secret_content" }]);
  assert.equal(done.source.revision, 6);
});

test("stopping between pages leaves the rest to the background watcher", async () => {
  let more = 0;
  const client = { reindexKnowledgeSource: async () => { more += 1; return page(1, 9, null); } };
  const done = await finishIndexing(client, "bot", page(5, 2, "next.md"), () => {}, () => true);
  assert.equal(more, 0);
  assert.equal(done.indexed, 5);
});
