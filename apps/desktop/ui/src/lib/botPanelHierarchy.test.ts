import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// The right panel reads top to bottom: who the bot is, its model and access level, what it is
// doing now, then the actions and the tabs (ux3-bot-panel).
const panel = readFileSync(new URL("../components/BotPanel.tsx", import.meta.url), "utf8");
const panelCss = readFileSync(new URL("../components/BotPanel.module.css", import.meta.url), "utf8");
const card = readFileSync(new URL("../components/BotComputerBlock.tsx", import.meta.url), "utf8");
const en = JSON.parse(readFileSync(new URL("../i18n/locales/en.json", import.meta.url), "utf8"));

test("the now row lives in the header, after the model line and before the actions", () => {
  const header = panel.slice(panel.indexOf("<header"), panel.indexOf("</header>"));
  const meta = header.indexOf("styles.meta");
  const now = header.indexOf('data-testid="bot-panel-now"');
  const actions = header.indexOf("styles.actions");
  assert.ok(meta > 0 && now > meta && actions > now, "meta, then now, then actions");
  assert.match(header, /data-testid="bot-panel-now-idle"/);
  const tabs = panel.slice(panel.indexOf('role="tabpanel"'));
  assert.doesNotMatch(tabs, /data-testid="bot-panel-now"/);
});

test("the panel shows the access level as one word, every level has one", () => {
  assert.match(panel, /<PermissionChip preset=\{preset\} short \/>/);
  for (const preset of ["full", "approve-destructive", "read-and-draft", "read-only", "custom"]) {
    assert.ok(en.accessLevel[preset]?.name, preset);
  }
});

test("the four tabs sit on one row (grid of equal columns, no wrapping)", () => {
  const tabs = panelCss.slice(panelCss.indexOf(".tabs {"), panelCss.indexOf("}", panelCss.indexOf(".tabs {")));
  assert.match(tabs, /display: grid/);
  assert.match(tabs, /grid-auto-flow: column/);
  assert.doesNotMatch(tabs, /flex-wrap: wrap/);
});

test("the live preview has one primary action and a compact quality menu", () => {
  assert.equal(card.match(/styles\.overPrimary/g)?.length, 1);
  assert.match(card, /data-testid="bot-computer-big"/);
  assert.match(card, /<QualitySelect /);
  assert.doesNotMatch(card, /<StreamQuality /);
  assert.match(card, /data-testid="bot-computer-close"[\s\S]{0,200}aria-label=\{t\("computerCard\.closeView"\)\}/);
});
