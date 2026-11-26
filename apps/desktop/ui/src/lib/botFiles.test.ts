import assert from "node:assert/strict";
import test from "node:test";
import { filesAndCommandsMissing, filesAndCommandsOps } from "./botFiles.ts";

// The allow list `bot setup` wrote for the General preset before files and commands were added.
const LEGACY = {
  tools: { allow: ["ask_user", "skill_*", "fs_read", "fs_list", "fs_search", "web_*", "mcp:*"] }
};

test("a bot whose allow list has only read-only file tools is missing files and commands", () => {
  assert.equal(filesAndCommandsMissing(LEGACY), true);
  assert.equal(filesAndCommandsMissing({}), false, "no allow list means every tool");
  assert.equal(filesAndCommandsMissing({ tools: { allow: ["fs_*", "shell_run"] } }), false);
  assert.equal(filesAndCommandsMissing({ tools: { exclude: ["shell_*"] } }), true);
});

test("a folder-only bot is not nagged about the shell it must never get", () => {
  const cfg = { files: { mode: "folder" }, tools: { allow: ["fs_*"] } };
  assert.equal(filesAndCommandsMissing(cfg), false);
});

test("the one button only adds the tools; the permission level and other lists stay untouched", () => {
  const ops = filesAndCommandsOps(LEGACY);
  assert.deepEqual(ops, [
    { op: "set", path: "/tools/allow", value: [...LEGACY.tools.allow, "fs_*", "checkpoint_*", "shell_run"] }
  ]);
  assert.ok(ops.every((o) => o.path.startsWith("/tools/")));
  assert.deepEqual(filesAndCommandsOps({}), []);
});

test("it removes only the exclude entries that hide these tools", () => {
  const ops = filesAndCommandsOps({ tools: { exclude: ["shell_*", "email_send", "fs_delete"] } });
  assert.deepEqual(ops, [{ op: "set", path: "/tools/exclude", value: ["email_send", "fs_delete"] }]);
  assert.deepEqual(filesAndCommandsOps({ tools: { exclude: ["shell_run"] } }), [
    { op: "unset", path: "/tools/exclude" }
  ]);
});
