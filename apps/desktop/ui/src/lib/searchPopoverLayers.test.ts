import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { createInertTracker } from "./inertStack.ts";
import { viewerKeyAction } from "./liveViewLayers.ts";
import { createModalStack } from "./modalStack.ts";
import { popoverEscapeAction, popoverPressAction } from "./popoverLayers.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const esc = { key: "Escape", shiftKey: false, defaultPrevented: false };
const tab = { key: "Tab", shiftKey: false, defaultPrevented: false };

test("the Search popover owns Escape only while it is the topmost layer", () => {
  const stack = createModalStack();
  const search = stack.next();
  stack.add(search);
  assert.equal(popoverEscapeAction(stack, search, "Escape"), "close");
  assert.equal(popoverEscapeAction(stack, search, "Tab"), null);
  assert.equal(popoverPressAction(stack, search, false), "close");
  assert.equal(popoverPressAction(stack, search, true), null, "a press on the field or the button keeps it open");

  // Settings (a Modal), the command palette and the live viewer all register through the same stack.
  for (const above of ["settings", "palette", "viewer"]) {
    const layer = stack.next();
    stack.add(layer);
    assert.equal(popoverEscapeAction(stack, search, "Escape"), null, `${above} keeps Escape`);
    assert.equal(popoverPressAction(stack, search, false), null, `a press inside ${above} does not close the popover`);
    stack.remove(layer);
    assert.equal(popoverEscapeAction(stack, search, "Escape"), "close", `${above} closed: the popover owns Escape again`);
  }
});

test("the live viewer yields Escape and Tab to a Search popover opened above it, and takes them back", () => {
  const stack = createModalStack();
  const viewer = stack.next();
  stack.add(viewer);
  const base = { stack, seq: viewer, interactive: false, stageFocused: false };
  assert.equal(viewerKeyAction({ ...base, event: esc }), "close");
  const search = stack.next();
  stack.add(search);
  assert.equal(viewerKeyAction({ ...base, event: esc }), null);
  assert.equal(viewerKeyAction({ ...base, event: tab }), null);
  stack.remove(search);
  assert.equal(viewerKeyAction({ ...base, event: esc }), "close");
});

test("the popover never touches #root inertness: any open/close order with a Modal leaves the app interactive", () => {
  const popover = read("../components/Popover.tsx");
  assert.doesNotMatch(popover, /appInert|inertStack|\.inert/);
  // popover (no token) opened before, between and after a Modal's token
  const log: boolean[] = [];
  const tracker = createInertTracker((v) => log.push(v));
  tracker.acquire(2); // Settings
  tracker.release(2);
  assert.equal(log.at(-1), false);
  assert.equal(tracker.holders, 0);
});

test("a dialog opened while the message search is open closes it (App wiring)", () => {
  const app = read("../App.tsx");
  // Every dialog the shell can open, including the bot dialogs (edit, profile, skills, add skill).
  assert.match(
    app,
    /const dialogOpen = settingsOpen !== null \|\| appsOpen \|\| newBotOpen \|\| recipeOpen \|\| helpOpen \|\| botEditOpen \|\| botProfileOpen \|\| skillsOpen \|\| addSkillOpen \|\| marketOpen \|\| vaultOpen;/
  );
  assert.match(app, /if \(dialogOpen\) setSearchOpen\(false\);/);
  const popover = read("../components/Popover.tsx");
  assert.match(popover, /popoverEscapeAction\(layerStack, seq, e\.key\)/);
  assert.match(popover, /popoverPressAction\(layerStack, seq, inside\)/);
});
