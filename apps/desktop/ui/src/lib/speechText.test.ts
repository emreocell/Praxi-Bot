import assert from "node:assert/strict";
import { test } from "node:test";
import { markdownToSpeech } from "./speechText.ts";

const labels = { code: "(code omitted)", table: "(table omitted)" };

test("code and tables are summarised, formatting marks removed", () => {
  const md = [
    "# Result",
    "",
    "**Done**: see [the docs](https://example.com/x) and `run`.",
    "- first",
    "- second",
    "```ts",
    "const secret = 1;",
    "```",
    "| a | b |",
    "|---|---|",
    "| 1 | 2 |",
    "Bye"
  ].join("\n");
  assert.equal(
    markdownToSpeech(md, labels),
    "Result\nDone: see the docs and run.\nfirst\nsecond\n(code omitted)\n(table omitted)\nBye"
  );
});

test("comparison signs are read aloud, real tags are still dropped", () => {
  assert.equal(
    markdownToSpeech("Eğer x < 10 ve y > 5 ise devam et.", labels),
    "Eğer x < 10 ve y > 5 ise devam et."
  );
  assert.equal(markdownToSpeech("a <b>bold</b> <https://example.com/x> c", labels), "a bold c");
});
