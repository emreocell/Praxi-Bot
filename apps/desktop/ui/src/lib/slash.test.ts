import assert from "node:assert/strict";
import { test } from "node:test";
import {
  applyCompletion,
  detectTrigger,
  mentionHandle,
  parseLeadingMention,
  parseSlashInput,
  SLASH_COMMANDS
} from "./slash.ts";

test("slash trigger only in the first word of the message", () => {
  assert.deepEqual(detectTrigger("/", 1), {
    kind: "slash",
    query: "",
    start: 0,
    end: 1
  });
  assert.deepEqual(detectTrigger("/ne", 3), {
    kind: "slash",
    query: "ne",
    start: 0,
    end: 3
  });
  // caret in the middle of the word: query is the part before it
  assert.equal(detectTrigger("/new", 2)?.query, "n");
  assert.equal(detectTrigger("/new ", 5), null);
  assert.equal(detectTrigger("/new arg", 8), null);
  assert.equal(detectTrigger("hello /ne", 9), null);
  assert.equal(detectTrigger("a/b", 3), null);
  assert.equal(detectTrigger("", 0), null);
  assert.equal(detectTrigger("/x", 0), null);
});

test("mention trigger at a word boundary, anywhere", () => {
  assert.deepEqual(detectTrigger("@at", 3), {
    kind: "mention",
    query: "at",
    start: 0,
    end: 3
  });
  assert.deepEqual(detectTrigger("ask @At", 7), {
    kind: "mention",
    query: "At",
    start: 4,
    end: 7
  });
  assert.equal(detectTrigger("mail me@x.com", 13), null);
  assert.equal(detectTrigger("@at ", 4), null);
  assert.equal(detectTrigger("x @ y", 3)?.query, "");
  assert.equal(detectTrigger("x @", 1), null);
  // a slash message can still carry a mention in a later word
  assert.equal(detectTrigger("/new @a", 7)?.kind, "mention");
});

test("applyCompletion replaces the trigger and puts the caret after it", () => {
  const t = detectTrigger("/ne", 3);
  assert.ok(t);
  assert.deepEqual(applyCompletion("/ne", t, "new"), {
    text: "/new ",
    caret: 5
  });
  const m = detectTrigger("ask @At please", 7);
  assert.ok(m);
  assert.deepEqual(applyCompletion("ask @At please", m, "Atlas"), {
    text: "ask @Atlas please",
    caret: 11
  });
  const end = detectTrigger("ask @A", 6);
  assert.ok(end);
  assert.deepEqual(applyCompletion("ask @A", end, "Atlas"), {
    text: "ask @Atlas ",
    caret: 11
  });
});

test("parseSlashInput", () => {
  assert.deepEqual(parseSlashInput("/new"), { name: "new", args: "" });
  assert.deepEqual(parseSlashInput("  /STOP now "), {
    name: "stop",
    args: "now"
  });
  assert.equal(parseSlashInput("not /new"), null);
  assert.equal(parseSlashInput("/"), null);
  assert.equal(parseSlashInput("//x"), null);
  assert.equal(parseSlashInput("/a/b"), null);
});

test("every slash command is lowercase and parses", () => {
  for (const c of SLASH_COMMANDS) {
    assert.equal(c.name, c.name.toLowerCase());
    assert.equal(parseSlashInput(`/${c.name}`)?.name, c.name);
  }
});

test("leading mention picks the bot and strips the handle", () => {
  const bots = [{ name: "Atlas" }, { name: "Deep Work" }, { name: "İpek" }];
  assert.equal(mentionHandle("Deep Work"), "DeepWork");
  assert.deepEqual(parseLeadingMention("@atlas do it\nplease", bots), {
    bot: bots[0],
    rest: "do it\nplease"
  });
  assert.equal(parseLeadingMention("@deepwork go", bots)?.bot, bots[1]);
  assert.equal(parseLeadingMention("@ipek hi", bots)?.bot, bots[2]);
  assert.equal(parseLeadingMention("@nobody hi", bots), null);
  assert.equal(parseLeadingMention("@atlas", bots), null);
  assert.equal(parseLeadingMention("hi @atlas", bots), null);
});
