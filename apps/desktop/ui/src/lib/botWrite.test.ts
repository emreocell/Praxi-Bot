import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../api/client.ts";
import type { Bot } from "../api/types.ts";
import { RollbackFailedError, botWriteMessage, writeBotProfile, writeThenApply } from "./botWrite.ts";

const bot = (version: number) => ({ id: "b1", name: "Atlas", version, config: {} }) as unknown as Bot;

test("a stale bot version reloads once and retries with the fresh version; a second conflict is a human sentence", async () => {
  const sent: Array<number | null | undefined> = [];
  let failures = 1;
  const client = {
    getBot: async () => bot(7),
    updateBot: async (_id: string, input: { expected_version?: number | null }) => {
      sent.push(input.expected_version);
      if (failures-- > 0) throw new ApiError(409, "version_conflict", "the bot changed since `expected_version`");
      return bot(8);
    }
  };
  const saved = await writeBotProfile(client, bot(3), (b) => ({ name: b.name, config: {} }) as never);
  assert.equal(saved.version, 8);
  assert.deepEqual(sent, [3, 7]);

  failures = 5;
  sent.length = 0;
  await assert.rejects(writeBotProfile(client, bot(3), () => ({ name: "x", config: {} }) as never), (e) => {
    const text = botWriteMessage(e, (k) => `tr:${k}`);
    assert.equal(text, "tr:bots.writeConflict");
    assert.doesNotMatch(text, /expected_version/);
    return true;
  });
  assert.equal(sent.length, 2, "exactly one retry");
});

test("a model that cannot get its engine step is put back, and the error still reaches the owner", async () => {
  const calls: string[] = [];
  const saved = bot(5);
  // The folder rule refused the engine step: the bot goes back to the model it had.
  await assert.rejects(
    writeThenApply(
      async () => (calls.push("write"), saved),
      async () => {
        calls.push("apply");
        throw new Error("a workspace folder is required");
      },
      async (b) => (calls.push(`undo:${b.version}`), b)
    ),
    /workspace folder/
  );
  assert.deepEqual(calls, ["write", "apply", "undo:5"]);
  calls.length = 0;
  const ok = await writeThenApply(async () => saved, async () => void calls.push("apply"), async () => void calls.push("undo"));
  assert.equal(ok, saved);
  assert.deepEqual(calls, ["apply"]);
});

test("when the old model cannot be put back either, the failure says so with both reasons", async () => {
  const saved = bot(5);
  await assert.rejects(
    writeThenApply(
      async () => saved,
      async () => { throw new Error("first"); },
      async () => { throw new Error("second"); }
    ),
    (e) => {
      assert.ok(e instanceof RollbackFailedError);
      assert.match(String((e.reason as Error).message), /first/);
      assert.match(String((e.undoError as Error).message), /second/);
      return true;
    }
  );
});
