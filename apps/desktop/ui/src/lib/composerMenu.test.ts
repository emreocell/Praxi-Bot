import assert from "node:assert/strict";
import { test } from "node:test";
import { BOT_SHAPE_IDS } from "ui/bot-shape";
import type { Bot, Chat } from "../api/types.ts";
import { botView } from "./botProfile.ts";
import { commandOptions, mentionOptions, optionGroups } from "./composerMenu.ts";

const bots: Bot[] = BOT_SHAPE_IDS.map((shape, i) => ({
  id: `bot-${i}`, name: i === 0 ? "Praxi" : `Bot ${i}`, created_at_ms: i, version: 1,
  config: { bot: { shape, color: "#abcdef", ...(i === 1 ? { avatar: "data:image/png;base64,AAAA" } : {}) } }
}));
const chat = (bot: Bot, time: number, pinned = false): Chat => ({
  id: `chat-${bot.id}`, chat_id: `chat-${bot.id}`, bot_id: bot.id, title: "Chat", created_at_ms: time, pinned
});

test("mention options preserve the exact sidebar identity for every bot shape and uploaded avatar", () => {
  for (const option of mentionOptions(bots, [], "")) {
    const sidebar = botView(bots.find((b) => b.id === option.id)!);
    assert.deepEqual(option.bot, sidebar);
    assert.equal(option.bot?.shape, sidebar.shape);
    assert.equal(option.bot?.avatar, sidebar.avatar);
  }
});

test("pinned and recent bots precede other bots; commands retain fuzzy highlights and recent priority", () => {
  const options = mentionOptions(bots.slice(0, 3), [chat(bots[1], 10), chat(bots[2], 1, true)], "");
  assert.deepEqual(options.map((o) => o.id), [bots[2].id, bots[1].id, bots[0].id]);
  assert.deepEqual(optionGroups(options), ["pinned", "recent", "bots"]);
  const commands = commandOptions("", ["settings"], (s) => s);
  assert.equal(commands[0].id, "settings");
  assert.equal(commands[0].group, "recent");
  assert.deepEqual(commandOptions("stp", [], (s) => s)[0].indices, [1, 2, 4]);
  assert.deepEqual(commandOptions("zzzzzz", [], (s) => s), []);
});
