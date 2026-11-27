import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("message actions: the icons come first and the time sits right of them, for both sides", () => {
  const tsx = read("../components/MessageActions.tsx");
  assert.match(tsx, /\{group\}\s*\{meta\}/);
  assert.doesNotMatch(tsx, /\{meta\}\s*\{group\}/, "the time must not lead the row");
  const css = read("../components/MessageActions.module.css");
  assert.match(css, /\.row \{[^}]*flex-wrap: wrap/, "on a very narrow column the time wraps instead of squeezing the icons");
  // The newest answer keeps its actions visible (as the run bar did); other rows keep hover / focus.
  assert.match(css, /\.actions\[data-latest="true"\] \{[^}]*opacity: 1/);
  assert.match(css, /\[data-message-row\]:hover\) \.actions/);
  assert.match(css, /@media \(hover: none\) \{\s*\.actions \{[^}]*opacity: 1/);
});

test("the run actions live in the newest answer's row; only a run without an answer keeps its own bar", () => {
  const chat = read("../views/ChatView.tsx");
  assert.match(chat, /runActions=\{actionsHost === `\$\{runId\}-\$\{item\.key\}` \? runActions : undefined\}/);
  assert.match(chat, /runActions=\{actionsHost === e\.key \? runActions : undefined\}/);
  assert.match(chat, /case "timeline":\s*return actionsHost === null \?/);
  for (const id of ["run-timeline", "remember-run"]) assert.ok(chat.includes(`testId="${id}"`), id);
  assert.match(read("../components/ChatMessageRow.tsx"), /runActions=\{runActions\}/);
});

test("model hint: a lightbulb with the text as its description, no permanent box", () => {
  const panel = read("../components/BotPanel.tsx");
  assert.match(panel, /<ModelHint onChangeModel=\{onOpenSettings\} \/>/);
  assert.doesNotMatch(panel, /liteHint|liteFullHint/, "the permanent box is gone");
  assert.match(panel, /data-hint-boundary/);
  const css = read("../components/BotPanel.module.css");
  assert.doesNotMatch(css, /\.liteHint/);

  const hint = read("../components/ModelHint.tsx");
  assert.match(hint, /aria-describedby=\{`\$\{id\}-text`\}/);
  assert.match(hint, /aria-expanded=\{open\}/);
  assert.match(hint, /t\("botPanel\.liteFullHint"\)/, "the text is unchanged");
  assert.match(hint, /t\("botPanel\.modelHint\.change"\)/);
  assert.match(hint, /:focus-visible/, "keyboard focus opens it, the focus a tap leaves behind does not");
  assert.match(hint, /pointerType === "mouse"/, "only a mouse hovers");
  assert.match(hint, /e\.key !== "Escape"/);
  const hintCss = read("../components/ModelHint.module.css");
  assert.match(hintCss, /\.panel \{[^}]*visibility: hidden/, "hidden at rest without leaving the page, so it still describes the button");
  assert.match(hintCss, /\.button \{[^}]*width: var\(--space-6\);\s*height: var\(--space-6\)/);
});

test("model hint strings exist in English and Turkish", () => {
  for (const lng of ["en", "tr"]) {
    const hint = JSON.parse(read(`../i18n/locales/${lng}.json`)).botPanel.modelHint;
    assert.ok(hint.label.length > 0 && hint.change.length > 0, lng);
  }
});
