import assert from "node:assert/strict";
import { test } from "node:test";
import {
  hasLanguage,
  highlight,
  MAX_HIGHLIGHT_CHARS,
  type Token
} from "./highlight.ts";

const join = (t: Token[]) => t.map((x) => x.text).join("");
const of = (t: Token[], type: Token["type"]) =>
  t.filter((x) => x.type === type).map((x) => x.text);

test("tokens always reassemble to the exact source", () => {
  const samples: [string, string][] = [
    ["const a = `x ${y}` + 'z'; // hi\n/* b */", "ts"],
    ['fn main() { println!("a\\"b"); }', "rust"],
    ["def f(x):\n  return x # c", "python"],
    ['{"a": [1, 2.5e3, true, null]}', "json"],
    ["echo $# # real comment", "bash"],
    ["unterminated 'string", "ts"],
    ["/* unterminated", "css"],
    ["???", "nonsense"],
    ["", "ts"]
  ];
  for (const [code, lang] of samples) {
    assert.equal(join(highlight(code, lang)), code, `${lang}: ${code}`);
  }
});

test("typescript: keywords, strings, numbers, comments", () => {
  const t = highlight("const n = 42; // note\nlet s = 'a';", "ts");
  assert.deepEqual(of(t, "keyword"), ["const", "let"]);
  assert.deepEqual(of(t, "number"), ["42"]);
  assert.deepEqual(of(t, "comment"), ["// note"]);
  assert.deepEqual(of(t, "string"), ["'a'"]);
});

test("escaped quote does not end a string", () => {
  assert.deepEqual(of(highlight('"a\\"b" x', "json"), "string"), ['"a\\"b"']);
});

test("rust and python specifics", () => {
  assert.deepEqual(of(highlight("let mut x = None;", "rust"), "keyword"), [
    "let",
    "mut"
  ]);
  assert.deepEqual(of(highlight("let mut x = None;", "rust"), "literal"), [
    "None"
  ]);
  assert.deepEqual(of(highlight("x = 1  # c", "py"), "comment"), ["# c"]);
});

test("json literals and numbers", () => {
  const t = highlight('{"a": true, "b": -1.5}', "json");
  assert.deepEqual(of(t, "literal"), ["true"]);
  assert.deepEqual(of(t, "number"), ["1.5"]);
});

test("shell hash inside a word is not a comment", () => {
  assert.deepEqual(of(highlight("echo $#", "sh"), "comment"), []);
  assert.deepEqual(of(highlight("ls # list", "sh"), "comment"), ["# list"]);
});

test("case-insensitive languages and aliases", () => {
  assert.deepEqual(of(highlight("SELECT 1 FROM t", "sql"), "keyword"), [
    "SELECT",
    "FROM"
  ]);
  assert.ok(hasLanguage("TSX"));
  assert.ok(!hasLanguage("brainfuck"));
});

test("markup in code stays text (tokens are data, never HTML)", () => {
  const t = highlight("<script>alert(1)</script>", "html");
  assert.equal(join(t), "<script>alert(1)</script>");
});

test("huge input is returned as one plain token", () => {
  const big = "a".repeat(MAX_HIGHLIGHT_CHARS + 1);
  assert.deepEqual(highlight(big, "ts"), [{ type: "plain", text: big }]);
});
