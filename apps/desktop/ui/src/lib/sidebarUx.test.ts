import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// The left rail (ux4): source-level checks; the rules themselves are unit-tested in railNav / quickFind.
const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("rail: every bot shows a status dot and says its status to a screen reader, also on the icon rail", () => {
  const app = read("../App.tsx");
  assert.match(app, /const dot = botDotStatus\(avatarState\);/);
  assert.match(app, /<span className=\{styles\.statusDot\} data-status=\{dot\} aria-hidden="true"/);
  assert.match(app, /aria-label=\{mini \? `\$\{view\.name\}, \$\{dotLabel\}` : undefined\}/);
  const css = read("../App.module.css");
  assert.match(css, /\.statusDot\[data-status="working"\]/);
  assert.match(css, /\.statusDot\[data-status="needsYou"\]/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\) \{\s*\.statusDot\[data-status="working"\] \{\s*animation: none;/);
});

test("rail: arrow keys move between rows (not on the reorder grip), and the list ends with a compact New bot row", () => {
  const app = read("../App.tsx");
  assert.match(app, /if \(!row\.matches\("\[data-rail-nav\]"\)/);
  assert.ok((app.match(/ data-rail-nav[ >\n]/g)?.length ?? 0) >= 5, "needs, notifications, schedules, bots and New bot are rail rows");
  assert.match(app, /data-testid="sidebar-new-bot"/);
  assert.doesNotMatch(read("../components/useBotReorder.ts"), /data-rail-nav/);
});

test("search: bots and chats by name come before the message results, and a bot opens its page", () => {
  const search = read("../components/MessageSearch.tsx");
  assert.match(search, /quickFind\(bots\.map/);
  assert.match(search, /if \(row\.kind === "bot"\) onOpenBot\?\.\(row\.botId\);/);
  assert.match(read("../App.tsx"), /onOpenChat=\{openChat\} onOpenBot=\{goToBot\} \/>/);
});
