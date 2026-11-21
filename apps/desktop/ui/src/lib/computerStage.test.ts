import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { computerPresence, mainBotLabel } from "./computerStage.ts";

test("presence: offline wins over busy, busy over online", () => {
  assert.equal(computerPresence({ offline: true, working: true }), "offline");
  assert.equal(computerPresence({ offline: false, working: true }), "busy");
  assert.equal(computerPresence({ offline: false, working: false }), "online");
});

test("main bot label ignores blank names", () => {
  assert.equal(mainBotLabel("  Atlas "), "Atlas");
  assert.equal(mainBotLabel("   "), null);
  assert.equal(mainBotLabel(undefined), null);
});

const panel = readFileSync(new URL("../components/BotPanel.tsx", import.meta.url), "utf8");
const card = readFileSync(new URL("../components/BotComputerBlock.tsx", import.meta.url), "utf8");
const app = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");

test("make-main is a labelled toggle beside the name, not a fourth row button", () => {
  const actions = panel.slice(panel.indexOf("styles.actions"), panel.indexOf("</header>"));
  assert.doesNotMatch(actions, /bot-panel-make-main/);
  assert.match(panel, /data-testid="bot-panel-make-main"[\s\S]*aria-pressed=\{isMain\}/);
  assert.match(panel, /styles\.mainBtn/);
});

test("computer card draws the device stage, not the gradient placeholder", () => {
  assert.doesNotMatch(card, /BotGradient/);
  assert.match(card, /data-testid="bot-computer-stage"/);
  assert.match(card, /data-presence=/);
  assert.match(card, /bot-computer-main/);
  assert.match(panel, /mainBotName=\{mainBotName\}/);
  assert.match(app, /mainBotName=/);
});

test("sidebar main mark is a quiet star with a label, not a text pill", () => {
  assert.match(app, /styles\.mainMark[^>]*role="img"[^>]*aria-label=\{t\("sidebar\.mainBotHint"\)\}/);
  assert.doesNotMatch(app, /<Star aria-hidden="true" \/>\{t\("sidebar\.mainBot"\)\}/);
});
