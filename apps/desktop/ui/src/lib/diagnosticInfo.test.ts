import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { INITIAL_DIAGNOSTIC_INFO, diagnosticInfoReducer, textToCopy } from "./diagnosticInfo.ts";

test("the copied text is exactly the previewed text, and only once it is ready", () => {
  let s = INITIAL_DIAGNOSTIC_INFO;
  assert.equal(textToCopy(s), null);
  s = diagnosticInfoReducer(s, { type: "load" });
  assert.equal(textToCopy(s), null);
  s = diagnosticInfoReducer(s, { type: "loaded", text: "# report\n- Version: 1" });
  assert.equal(textToCopy(s), "# report\n- Version: 1");
  // A late answer after a reset must not resurrect a preview.
  s = diagnosticInfoReducer(s, { type: "reset" });
  s = diagnosticInfoReducer(s, { type: "loaded", text: "late" });
  assert.equal(textToCopy(s), null);
});

test("a failed load keeps nothing copyable", () => {
  let s = diagnosticInfoReducer(INITIAL_DIAGNOSTIC_INFO, { type: "load" });
  s = diagnosticInfoReducer(s, { type: "failed", error: new Error("429") });
  assert.equal(s.phase, "error");
  assert.equal(textToCopy(s), null);
});

test("the Help card copies the previewed text and never refetches on Copy", () => {
  const src = readFileSync(new URL("../components/DiagnosticInfo.tsx", import.meta.url), "utf8");
  assert.equal([...src.matchAll(/client\.doctorReport\(/g)].length, 1);
  assert.match(src, /clipboard\.writeText\(text\)/);
  assert.match(src, /textToCopy\(state\)/);
});
