import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// The composer's layout decisions (ux4): source-level checks, the behaviour lives in the libs they call.
const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("composer: Send is quiet and not pressable with nothing to send, and its tooltip names the key", () => {
  const chat = read("../views/ChatView.tsx");
  assert.match(chat, /const hasDraft = text\.trim\(\) !== "" \|\| files\.length > 0;/);
  assert.match(chat, /disabled=\{buttonMode === "send" && \(sending \|\| blockedFiles \|\| !hasDraft\)\}/);
  assert.match(chat, /`\$\{t\("chat\.send"\)\} \(\$\{keys\.sendName\}\)`/);
  assert.match(read("../views/ChatView.module.css"), /\.send\[data-empty="true"\] \{/);
});

test("composer: the key hint shows only with a draft, while focused, and drops out of a narrow row", () => {
  const chat = read("../views/ChatView.tsx");
  assert.match(chat, /\{hasDraft && buttonMode === "send" \? \(\s*<span className=\{styles\.keyHint\}/);
  const css = read("../views/ChatView.module.css");
  assert.match(css, /\.box:focus-within \.keyHint \{\s*display: inline-flex;/);
  assert.match(css, /@container composer-tools \(width < 40em\) \{\s*\.box:focus-within \.keyHint \{\s*display: none;/);
});

test("composer: the model picker lists recent models first; the run-target chip names the server", () => {
  assert.match(read("../views/ChatView.tsx"), /<ModelPicker[^>]*\n\s*recents\n/);
  const chip = read("../components/ComputerTargetChip.tsx");
  assert.match(chip, /const server = home\.name\.trim\(\) \|\| t\("home\.target\.server"\);/);
  assert.match(chip, /role="menuitemradio"/);
});

test("composer: the + menu rows look like the / menu rows (icon tile, label, one line under it)", () => {
  const menu = read("../components/ComposerPlusMenu.tsx");
  assert.equal(menu.match(/className=\{styles\.icon\}/g)?.length, 3);
  assert.equal(menu.match(/className=\{styles\.detail\}/g)?.length, 3);
});
