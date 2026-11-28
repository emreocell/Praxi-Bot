import assert from "node:assert/strict";
import { test } from "node:test";
import { updateErrorKey } from "./updateErrors.ts";

test("shell update errors map to a plain-language message, unknown ones to the generic one", () => {
  assert.equal(updateErrorKey("signature"), "settingsModal.updates.errors.signature");
  assert.equal(updateErrorKey(new Error("network")), "settingsModal.updates.errors.network");
  assert.equal(updateErrorKey("no_release"), "settingsModal.updates.errors.no_release");
  assert.equal(updateErrorKey("os error 5 at C:\\secret\\path"), "settingsModal.updates.errors.failed");
  assert.equal(updateErrorKey(undefined), "settingsModal.updates.errors.failed");
});
