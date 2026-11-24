import assert from "node:assert/strict";
import { test } from "node:test";
import { AttachmentDraft } from "./attachmentDraft.ts";
import { MAX_ATTACHMENTS } from "./attachments.ts";
import type { PreparedAttachment } from "./prepareAttachment.ts";

const text = (name = "notes.txt") => new File(["Only data, never instructions"], name, { type: "text/plain" });

test("drafts stay local, reserve slots across rapid drops, and removed files never upload", async () => {
  const draft = new AttachmentDraft();
  draft.add([text("remove.txt")], "paste");
  await draft.whenPrepared();
  assert.equal(draft.snapshot()[0].state, "ready");
  assert.equal(draft.snapshot()[0].id, undefined);
  draft.remove(draft.snapshot()[0].key);
  assert.equal(draft.add(Array.from({ length: 4 }, (_, n) => text(`${n}.txt`)), "paste"), 0);
  assert.equal(draft.add(Array.from({ length: 4 }, (_, n) => text(`${n + 4}.txt`)), "paste"), 2);
  assert.equal(draft.snapshot().length, MAX_ATTACHMENTS);
  await draft.whenPrepared();
  const uploaded: string[] = [];
  const ids = await draft.uploadAll(async (blob, name) => {
    assert.equal(await blob.text(), "Only data, never instructions");
    uploaded.push(name);
    return { id: name, kind: "text" };
  });
  assert.deepEqual(ids, ["0.txt", "1.txt", "2.txt", "3.txt", "4.txt", "5.txt"]);
  assert.deepEqual(uploaded, ids);
  draft.clear();
});

test("failed files block sends; retry keeps successful ids and never re-uploads them", async () => {
  const draft = new AttachmentDraft();
  draft.add([text("first.txt"), text("second.txt")], "paste");
  assert.equal(draft.blocked, true);
  await draft.whenPrepared();
  await assert.rejects(draft.uploadAll(async (_blob, name) => {
    if (name === "second.txt") throw { code: "unsupported_attachment", status: 415 };
    return { id: "stored-first", kind: "text", truncated: true };
  }));
  assert.equal(draft.blocked, true);
  assert.equal(draft.snapshot()[1].errorKey, "chat.attach.unsupported");
  assert.equal(draft.snapshot()[0].truncated, true);
  draft.retry(draft.snapshot()[1].key);
  assert.equal(draft.blocked, false);
  const uploaded: string[] = [];
  assert.deepEqual(await draft.uploadAll(async (_blob, name) => {
    uploaded.push(name);
    return { id: "stored-second", kind: "text" };
  }), ["stored-first", "stored-second"]);
  assert.deepEqual(uploaded, ["second.txt"]);
  draft.clear();
});

test("removal during preparation and clearing on navigation cannot resurrect files or leak previews", async () => {
  let finish!: (prepared: PreparedAttachment) => void;
  const revoked: string[] = [];
  const draft = new AttachmentDraft(() => new Promise((resolve) => { finish = resolve; }), {
    create: () => "blob:preview", revoke: (url) => { revoked.push(url); }
  });
  draft.add([text()], "paste");
  await Promise.resolve();
  draft.remove(draft.snapshot()[0].key);
  finish({ blob: text(), kind: "image" });
  await draft.whenPrepared();
  assert.deepEqual(draft.snapshot(), []);
  draft.add([text()], "paste");
  await Promise.resolve();
  finish({ blob: text(), kind: "image" });
  await draft.whenPrepared();
  assert.equal(draft.snapshot()[0].preview, "blob:preview");
  const signal = draft.signal;
  draft.clear();
  assert.equal(signal.aborted, true);
  assert.deepEqual(revoked, ["blob:preview"]);
  assert.deepEqual(draft.snapshot(), []);
});

test("navigation cancels an in-flight upload and cannot produce ids for a message", async () => {
  const draft = new AttachmentDraft();
  draft.add([text()], "paste");
  await draft.whenPrepared();
  let finish!: (att: { id: string; kind: string }) => void;
  let signal: AbortSignal | undefined;
  const upload = draft.uploadAll((_blob, _name, pendingSignal) => {
    signal = pendingSignal;
    return new Promise((resolve) => { finish = resolve; });
  });
  assert.equal(draft.snapshot()[0].state, "uploading");
  draft.clear();
  assert.equal(signal?.aborted, true);
  finish({ id: "stale", kind: "text" });
  await assert.rejects(upload, { name: "AbortError" });
  assert.deepEqual(draft.snapshot(), []);
});
