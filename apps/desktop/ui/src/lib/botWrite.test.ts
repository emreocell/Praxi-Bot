import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../api/client.ts";
import type { Bot } from "../api/types.ts";
import { NoPreviousModelError, RollbackFailedError, botWriteMessage, previousExecutor, switchExecutor, writeBotProfile, writeThenApply } from "./botWrite.ts";

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

const botOn = (executor: string | undefined, version = 1) =>
  ({ id: "b1", name: "Atlas", version, config: executor === undefined ? {} : { models: { executor } } }) as unknown as Bot;

test("switching a model starts from the fresh bot and puts that model back when the engine step fails", async () => {
  const written: string[] = [];
  const client = {
    getBot: async () => botOn("openai/gpt-5", 9),
    updateBot: async (_id: string, input: { config: { models: { executor: string } }; expected_version?: number | null }) => {
      written.push(`${input.config.models.executor}@${input.expected_version}`);
      return botOn(input.config.models.executor, (input.expected_version ?? 0) + 1);
    }
  };
  const setExecutor = (executor: string) => (b: Bot) => ({ name: b.name, config: { models: { executor } } }) as never;
  assert.equal(previousExecutor(botOn("openai/gpt-5")), "openai/gpt-5");
  await assert.rejects(
    switchExecutor({ client, fresh: botOn("openai/gpt-5", 9), ref: "cli:agy/x", setExecutor, apply: async () => { throw new Error("engine refused"); } }),
    /engine refused/
  );
  // New model written on the fresh version, then the previous one back on the new version.
  assert.deepEqual(written, ["cli:agy/x@9", "openai/gpt-5@10"]);
});

test("a bot with no current model cannot be switched: nothing is written and no no-op undo is used", async () => {
  for (const fresh of [botOn(undefined), botOn(""), botOn("   ")]) {
    let writes = 0;
    const client = { getBot: async () => fresh, updateBot: async () => (writes += 1, fresh) };
    assert.equal(previousExecutor(fresh), null);
    await assert.rejects(
      switchExecutor({ client, fresh, ref: "cli:agy/x", setExecutor: () => () => ({}) as never, apply: async () => undefined }),
      (e) => e instanceof NoPreviousModelError
    );
    assert.equal(writes, 0);
  }
});

test("a failing rollback of a switch is reported, never swallowed", async () => {
  let calls = 0;
  const client = {
    getBot: async () => botOn("openai/gpt-5"),
    updateBot: async () => {
      calls += 1;
      if (calls > 1) throw new Error("daemon unreachable");
      return botOn("cli:agy/x", 2);
    }
  };
  await assert.rejects(
    switchExecutor({ client, fresh: botOn("openai/gpt-5"), ref: "cli:agy/x", setExecutor: () => () => ({}) as never, apply: async () => { throw new Error("engine refused"); } }),
    (e) => e instanceof RollbackFailedError && /engine refused/.test(String((e.reason as Error).message)) && /unreachable/.test(String((e.undoError as Error).message))
  );
});
