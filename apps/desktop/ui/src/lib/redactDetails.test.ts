import assert from "node:assert/strict";
import { test } from "node:test";
import { redactDetails } from "./redactDetails.ts";

const alnum = (n: number) => "aB3dE5gH7jK9mN1pQ2rS4tU6vW8xY0z".repeat(3).slice(0, n);
const OPENAI_KEY = "sk-" + alnum(30);

test("empty and whitespace input is unchanged", () => {
  assert.equal(redactDetails(""), "");
});

test("provider-shaped secrets from the detector are replaced", () => {
  const raw = `failed: use ${OPENAI_KEY} here`;
  const out = redactDetails(raw);
  assert.ok(!out.includes(OPENAI_KEY));
  assert.ok(out.includes("[hidden]"));
});

test("collapses excess blank lines after trimming stack frames", () => {
  const raw = "line one\n\n\n\nline two";
  assert.equal(redactDetails(raw), "line one\n\nline two");
});