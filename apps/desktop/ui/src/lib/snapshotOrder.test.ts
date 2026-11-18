import assert from "node:assert/strict";
import { test } from "node:test";
import { admitSnapshot, createSnapshotSequencer } from "./snapshotOrder.ts";

test("tickets are strictly increasing and accepted in order", () => {
  const s = createSnapshotSequencer();
  const a = s.issue();
  const b = s.issue();
  assert.ok(b > a);
  assert.equal(s.accept(a), true);
  assert.equal(s.accept(b), true);
});

test("an older GET that resolves after a newer one is dropped", () => {
  const s = createSnapshotSequencer();
  const older = s.issue();
  const newer = s.issue();
  assert.equal(s.accept(newer), true);
  assert.equal(s.accept(older), false);
});

test("an action response invalidates every GET issued before it", () => {
  const s = createSnapshotSequencer();
  const inFlight = s.issue();
  s.markFresh();
  assert.equal(s.accept(inFlight), false);
  const after = s.issue();
  assert.equal(s.accept(after), true);
});

test("admitSnapshot: untagged snapshots always win, ticketed ones follow accept", () => {
  const s = createSnapshotSequencer();
  const get = s.issue();
  assert.equal(admitSnapshot(s, undefined), true);
  assert.equal(admitSnapshot(s, get), false);
  const later = s.issue();
  assert.equal(admitSnapshot(s, later), true);
});

test("the same ticket may be applied again (re-delivery is not older)", () => {
  const s = createSnapshotSequencer();
  const t = s.issue();
  assert.equal(s.accept(t), true);
  assert.equal(s.accept(t), true);
});
