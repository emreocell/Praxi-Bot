import assert from "node:assert/strict";
import { test } from "node:test";
import {
  applyCompletion,
  detectTrigger,
  mentionHandle,
  appCommandFor,
  APP_COMMANDS,
  LOCAL_COMMANDS,
  MISSION_COMMAND,
  missionCommandGoal,
  missionCommandWord,
  parseSlashInput,
  SLASH_COMMANDS,
  unknownSlashCommand
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

test("mention handle is the bot name without whitespace", () => {
  assert.equal(mentionHandle("Deep Work"), "DeepWork");
});

test("an app command is exactly /name; arguments or other text go to the bot", () => {
  assert.equal(appCommandFor("/help")?.target, "help");
  assert.equal(appCommandFor("  /Models ")?.target, "ai");
  assert.equal(appCommandFor("/help me with my taxes"), null);
  assert.equal(appCommandFor("/nope"), null);
  for (const c of APP_COMMANDS) {
    assert.ok(!SLASH_COMMANDS.some((d) => d.name === c.name), `${c.name} clashes with a daemon command`);
  }
});

test("a lone unknown /word is flagged, known commands, arguments and paths are not", () => {
  assert.equal(unknownSlashCommand("/foobar"), "foobar");
  assert.equal(unknownSlashCommand("  /FooBar "), "foobar");
  for (const ok of ["/new", "/stop", "/help", "/export", "/workflow", "/foobar with text", "/usr/bin/ls", "hello /foobar"]) {
    assert.equal(unknownSlashCommand(ok), null, ok);
  }
});

test("/mission and /görev carry the job; a lone one is known, not a paid model call", () => {
  assert.equal(missionCommandGoal("/mission Sign up on 20 sites"), "Sign up on 20 sites");
  assert.equal(missionCommandGoal("  /Görev 20 siteye kaydol\nve raporla "), "20 siteye kaydol\nve raporla");
  assert.equal(missionCommandGoal("/gorev x"), "x");
  assert.equal(missionCommandGoal("/mission"), "");
  assert.equal(missionCommandGoal("/missions x"), null);
  assert.equal(missionCommandGoal("start a mission"), null);
  for (const lone of ["/mission", "/görev", "/gorev"]) assert.equal(unknownSlashCommand(lone), null, lone);
  assert.equal(missionCommandWord("tr-TR"), "görev");
  assert.equal(missionCommandWord("en"), "mission");
  assert.ok(LOCAL_COMMANDS.includes(MISSION_COMMAND));
});
