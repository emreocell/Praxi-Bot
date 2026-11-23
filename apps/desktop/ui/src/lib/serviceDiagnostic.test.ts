import assert from "node:assert/strict";
import test from "node:test";
import { buildServiceDiagnostic } from "./serviceDiagnostic.ts";

test("diagnostic text works without shell details and carries the phase", () => {
  const text = buildServiceDiagnostic(null, "error");
  assert.match(text, /app phase: error/);
  assert.match(text, /no start-up details/);
});
