import { strict as assert } from "node:assert";
import { test } from "node:test";
import { hasExternalAddress, splitServerText } from "./serverText.ts";

const external = (text: string) => splitServerText(text).filter((p) => p.external).map((p) => p.text);

test("ordinary sentences have no external address", () => {
  const parts = splitServerText("Which region should I use, e.g. Europe? Version 1.2.3 is fine.");
  assert.equal(hasExternalAddress(parts), false);
  assert.equal(parts.map((p) => p.text).join(""), "Which region should I use, e.g. Europe? Version 1.2.3 is fine.");
});

test("scheme, www and bare-domain addresses are marked, without their trailing punctuation", () => {
  assert.deepEqual(external("Sign in at https://example.com/login, then continue."), ["https://example.com/login"]);
  assert.deepEqual(external("Open praxi://approve/all now"), ["praxi://approve/all"]);
  assert.deepEqual(external("Go to www.example.org."), ["www.example.org"]);
  assert.deepEqual(external("Mail mailto:help@example.com (urgent)"), ["mailto:help@example.com"]);
  assert.deepEqual(external("See praxi-help.example.net/verify?code=1!"), ["praxi-help.example.net/verify?code=1"]);
});

test("an address with letters outside ASCII is flagged as unusual", () => {
  const [lookalike] = splitServerText("Visit аpple.com today").filter((p) => p.external);
  assert.equal(lookalike.text, "аpple.com");
  assert.equal(lookalike.unusual, true);
  const [ascii] = splitServerText("Visit apple.com today").filter((p) => p.external);
  assert.equal(ascii.unusual, false);
});

test("markup stays text and hidden characters are written out", () => {
  const parts = splitServerText("<b>Choose</b> <a href=\"https://evil.test\">here</a>‮");
  assert.equal(parts.map((p) => p.text).join(""), "<b>Choose</b> <a href=\"https://evil.test\">here</a><U+202E>");
  assert.deepEqual(external("<a href=\"https://evil.test\">here</a>"), ["https://evil.test"]);
});
