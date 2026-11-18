import assert from "node:assert/strict";
import { test } from "node:test";
import { humanLine } from "./humanError.ts";
import { createChatFor, newChatFailureNeedsToast } from "./newChat.ts";

test("a created chat is returned after the chat list is refreshed", async () => {
  const calls: string[] = [];
  const client = {
    createChat: async (botId: string) => {
      calls.push(`create:${botId}`);
      return { id: "chat-1" } as never;
    }
  };
  const r = await createChatFor(client, "bot-1", async () => void calls.push("refresh"));
  assert.deepEqual(r, { ok: true, chatId: "chat-1" });
  assert.deepEqual(calls, ["create:bot-1", "refresh"]);
});

test("a failing list refresh does not fail the new chat", async () => {
  const client = { createChat: async () => ({ id: "c" }) as never };
  const r = await createChatFor(client, "b", () => Promise.reject(new Error("offline")));
  assert.deepEqual(r, { ok: true, chatId: "c" });
});

test("a failing create becomes a message, not an exception, and skips the refresh", async () => {
  const err = new Error("boom");
  let refreshed = false;
  const client = {
    createChat: async () => {
      throw err;
    }
  };
  const r = await createChatFor(client, "b", async () => {
    refreshed = true;
  });
  assert.deepEqual(r, { ok: false, message: humanLine(err) });
  assert.equal(refreshed, false);
});

test("only the bot route renders the failure inline; every other route needs a toast", () => {
  assert.equal(newChatFailureNeedsToast("bot"), false);
  assert.equal(newChatFailureNeedsToast("chat"), true);
  assert.equal(newChatFailureNeedsToast("home"), true);
});
