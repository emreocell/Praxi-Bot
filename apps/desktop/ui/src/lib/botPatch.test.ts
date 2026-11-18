import assert from "node:assert/strict";
import { test } from "node:test";
import type { Bot } from "../api/types.ts";
import { applyPatchOps, conflictBot, isEmptyPatch, patchedCategories, profilePatch } from "./botPatch.ts";
import { type BotDraft, draftFromBot } from "./botProfile.ts";

const base = (): BotDraft => draftFromBot();

test("an unchanged draft produces an empty patch", () => {
  const p = profilePatch(base(), base());
  assert.deepEqual(p.ops, []);
  assert.equal(isEmptyPatch(p), true);
});

test("only touched profile fields are mentioned", () => {
  const opened = base();
  const draft = { ...base(), name: "  Rex ", persona: "calm", color: opened.color.toUpperCase() };
  const p = profilePatch(opened, draft);
  assert.equal(p.name, "Rex");
  assert.deepEqual(p.ops, [
    { op: "set", path: "/bot/name", value: "Rex" },
    { op: "set", path: "/bot/persona", value: "calm" }
  ]);
});

test("avatar set and cleared", () => {
  const opened = base();
  assert.deepEqual(profilePatch(opened, { ...opened, avatar: "data:x" }).ops, [
    { op: "set", path: "/bot/avatar", value: "data:x" }
  ]);
  assert.deepEqual(profilePatch({ ...opened, avatar: "data:x" }, opened).ops, [
    { op: "unset", path: "/bot/avatar" }
  ]);
});

test("model changes set or unset, fallback compares as a list", () => {
  const opened = base();
  const draft = {
    ...opened,
    executor: { provider: "p", model: "m" },
    planner: { provider: "", model: "" },
    fallback: "a/b, a/b, c/d"
  };
  const withPlanner = { ...opened, planner: { provider: "x", model: "y" } };
  const ops = profilePatch(withPlanner, draft).ops;
  assert.deepEqual(ops, [
    { op: "set", path: "/models/executor", value: "p/m" },
    { op: "unset", path: "/models/planner" },
    { op: "set", path: "/models/fallback", value: ["a/b", "c/d"] }
  ]);
  const cleared = profilePatch({ ...opened, fallback: "a/b" }, { ...opened, fallback: " " }).ops;
  assert.deepEqual(cleared, [{ op: "unset", path: "/models/fallback" }]);
});

test("a preset choice replaces the policy; a category edit rewrites only its level", () => {
  const opened = base();
  assert.deepEqual(profilePatch(opened, { ...opened, preset: "custom", categories: { ...opened.categories } }).ops, []);
  const other = opened.preset === "approve-destructive" ? "read-only" : "approve-destructive";
  const p = profilePatch(opened, { ...opened, preset: other as BotDraft["preset"] });
  assert.deepEqual(p.ops, [{ op: "set", path: "/permissions", value: { preset: other } }]);

  const level = opened.categories.shell === "deny" ? "ask" : "deny";
  const q = profilePatch(opened, { ...opened, preset: "custom", categories: { ...opened.categories, shell: level } });
  assert.deepEqual(q.ops, [{ op: "set", path: "/permissions/categories/shell/level", value: { level } }]);
  assert.deepEqual(patchedCategories(q), ["shell"]);
});

test("applyPatchOps sets with parents, unsets, escapes and does not mutate the input", () => {
  const cfg = { bot: { name: "a" }, keep: 1 };
  const next = applyPatchOps(cfg, [
    { op: "set", path: "/bot/name", value: "b" },
    { op: "set", path: "/models/fallback", value: ["x"] },
    { op: "set", path: "/a~1b/c~0d", value: 1 },
    { op: "unset", path: "/missing/deep" },
    { op: "unset", path: "/keep" }
  ]);
  assert.deepEqual(next, { bot: { name: "b" }, models: { fallback: ["x"] }, "a/b": { "c~d": 1 } });
  assert.deepEqual(cfg, { bot: { name: "a" }, keep: 1 });
  assert.deepEqual(applyPatchOps(undefined, [{ op: "set", path: "/x", value: 1 }]), { x: 1 });
});

test("applyPatchOps rejects bad paths", () => {
  assert.throws(() => applyPatchOps({}, [{ op: "set", path: "x", value: 1 }]), /must start with/);
  assert.throws(() => applyPatchOps({}, [{ op: "set", path: "/a//b", value: 1 }]), /empty segment/);
  assert.throws(() => applyPatchOps({ a: 1 }, [{ op: "set", path: "/a/b", value: 1 }]), /non-object/);
});

test("conflictBot returns the current bot of a version_conflict 409 only", () => {
  const current = { id: "b" } as unknown as Bot;
  assert.equal(conflictBot({ status: 409, code: "version_conflict", body: { current } }), current);
  assert.equal(conflictBot({ status: 409, code: "other", body: { current } }), null);
  assert.equal(conflictBot({ status: 500, code: "version_conflict" }), null);
  assert.equal(conflictBot({ status: 409, code: "version_conflict" }), null);
  assert.equal(conflictBot(null), null);
});
