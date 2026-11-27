import assert from "node:assert/strict";
import { test } from "node:test";
import { isPlaceholderModel, isPlaceholderProvider } from "./placeholderModel.ts";

test("validation-only provider and model refs are detected", () => {
  for (const id of [
    "validation-only",
    "validation_placeholder",
    "VALIDATION-ONLY",
    "validation only",
    "foo/validation-only-bar"
  ]) {
    assert.equal(isPlaceholderProvider(id), true, id);
    assert.equal(isPlaceholderModel(id), true, id);
  }
  for (const id of ["anthropic", "claude-sonnet-5-5", "ollama/qwen3:8b", "", null, undefined]) {
    assert.equal(isPlaceholderProvider(id), false, String(id));
    assert.equal(isPlaceholderModel(id), false, String(id));
  }
});