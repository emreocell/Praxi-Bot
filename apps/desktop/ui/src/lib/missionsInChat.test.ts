import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const block = read("../components/MissionBlock.tsx");
const header = read("../components/MissionChatHeader.tsx");
const chat = read("../views/ChatView.tsx");
const locale = (lang: string) => JSON.parse(read(`../i18n/locales/${lang}.json`)) as Record<string, unknown>;
const lookup = (tree: Record<string, unknown>, key: string): unknown =>
  key.split(".").reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], tree);

test("the bot panel lists missions one line each and starts nothing itself", () => {
  const panel = block.slice(block.indexOf("export function MissionBlock("), block.indexOf("export function MissionChipView("));
  assert.doesNotMatch(panel, /startMission|<textarea|NeedsYou|Digest|MissionSteer/);
  assert.match(panel, /onClick=\{\(\) => requestOpenChat\(m\.chat_id\)\}/);
  assert.match(panel, /<MissionChipView state=\{m\.state\} \/>/);
  assert.match(panel, /progressShort\(m\)/);
  // An empty panel points at the chat: the button types "/mission " (or "/görev ") into the message box.
  assert.match(panel, /requestCompose\(`\/\$\{missionCommandWord\(i18n\.language\)\}`\)/);
});

test("/mission starts the mission from the chat, before the queue, with no confirmation card", () => {
  const send = chat.slice(chat.indexOf("const send = async"), chat.indexOf("const onKeyDown"));
  const at = send.indexOf("missionCommandGoal(body)");
  assert.ok(at > send.indexOf("kindsToWarn(text"), "the secrets check still runs first");
  assert.ok(at < send.indexOf("queuesLocally("), "a mission never waits in the local queue");
  const branch = send.slice(at, send.indexOf("    return;\n    }\n", at));
  assert.match(branch, /client\.startMission\(\{ bot_id: chat\.bot_id, goal: missionGoal \}\)/);
  assert.match(branch, /rememberChatMission\(chatId, started\.mission\.id\)/);
  assert.doesNotMatch(branch, /confirm|approval|setPending/i);
});

test("a mission started from a chat is a card in its stream, and the mission's own chat can pause and stop it", () => {
  assert.match(chat, /out\.push\(\{ kind: "mission", key: `mission-\$\{mission\.id\}`, ts: mission\.started_at_ms, mission \}\)/);
  assert.match(chat, /<MissionStartedCard mission=\{e\.mission\} onChanged=\{chatMission\.refresh\} \/>/);
  // The bot's own mission_create (a natural sentence) refreshes the missions so its card appears too.
  assert.match(chat, /i\.name === "mission_create"/);
  const card = header.slice(header.indexOf("export function MissionStartedCard("));
  assert.match(card, /<MissionSteer /);
  assert.match(card, /requestOpenChat\(m\.chat_id\)/);
  const head = header.slice(header.indexOf("export function MissionChatHeader("), header.indexOf("const OUTCOME_ICONS"));
  assert.match(head, /<MissionSteer /);
});

test("every mission-in-chat text exists in English and Turkish", () => {
  const keys = new Set(
    [block, header, chat].flatMap((src) => [...src.matchAll(/t\("(missionChat\.[a-zA-Z.]+)"/g)].map((m) => m[1]))
  );
  keys.add("missionChat.slash");
  assert.ok(keys.size >= 10);
  for (const lang of ["en", "tr"]) {
    for (const key of keys) {
      const value = lookup(locale(lang), key) ?? lookup(locale(lang), `${key}_other`);
      assert.equal(typeof value, "string", `${lang}: ${key}`);
    }
  }
  assert.match(String(lookup(locale("tr"), "missionChat.slash")), /\/görev/);
});
