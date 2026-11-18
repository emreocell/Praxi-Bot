import assert from "node:assert/strict";
import { test } from "node:test";
import { createModalStack } from "./modalStack.ts";
import { popoverEscapeAction, popoverPressAction } from "./popoverLayers.ts";

test("Escape closes only the topmost layer", () => {
  const stack = createModalStack();
  const dialog = stack.next();
  stack.add(dialog);
  const popover = stack.next();
  stack.add(popover);
  assert.equal(popoverEscapeAction(stack, popover, "Escape"), "close");
  assert.equal(popoverEscapeAction(stack, dialog, "Escape"), null);
  assert.equal(popoverEscapeAction(stack, popover, "Enter"), null);
});

test("a dialog opened above a popover keeps the key", () => {
  const stack = createModalStack();
  const popover = stack.next();
  stack.add(popover);
  const dialog = stack.next();
  stack.add(dialog);
  assert.equal(popoverEscapeAction(stack, popover, "Escape"), null);
  stack.remove(dialog);
  assert.equal(popoverEscapeAction(stack, popover, "Escape"), "close");
});

test("an outside press closes only while the popover is on top", () => {
  const stack = createModalStack();
  const popover = stack.next();
  stack.add(popover);
  assert.equal(popoverPressAction(stack, popover, false), "close");
  assert.equal(popoverPressAction(stack, popover, true), null);
  const dialog = stack.next();
  stack.add(dialog);
  assert.equal(popoverPressAction(stack, popover, false), null);
});
