import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type Block,
  type Inline,
  parseInline,
  parseMarkdown,
  sanitizeUrl
} from "./markdown.ts";

/** Every link the AST holds, to assert nothing unsafe survives. */
function links(nodes: (Inline | Block)[], out: string[] = []): string[] {
  for (const n of nodes) {
    if (n.t === "link") {
      out.push(n.href);
    }
    if ("children" in n) {
      links(n.children as (Inline | Block)[], out);
    }
    if (n.t === "list") {
      for (const it of n.items) {
        links(it.children, out);
      }
    }
    if (n.t === "table") {
      for (const c of [...n.head, ...n.rows.flat()]) {
        links(c, out);
      }
    }
  }
  return out;
}

test("sanitizeUrl allows only http, https and mailto", () => {
  assert.equal(
    sanitizeUrl("https://example.com/a?b=1"),
    "https://example.com/a?b=1"
  );
  assert.equal(sanitizeUrl("HTTP://example.com"), "HTTP://example.com");
  assert.equal(sanitizeUrl("mailto:a@b.co"), "mailto:a@b.co");
  assert.equal(sanitizeUrl("javascript:alert(1)"), null);
  assert.equal(sanitizeUrl("JaVaScRiPt:alert(1)"), null);
  assert.equal(sanitizeUrl("java\tscript:alert(1)"), null);
  assert.equal(sanitizeUrl("  javascript:alert(1)"), null);
  assert.equal(sanitizeUrl("data:text/html,<script>1</script>"), null);
  assert.equal(sanitizeUrl("vbscript:x"), null);
  assert.equal(sanitizeUrl("file:///C:/Windows/win.ini"), null);
  assert.equal(sanitizeUrl("//evil.example"), null);
  assert.equal(sanitizeUrl("/relative"), null);
  assert.equal(sanitizeUrl("#anchor"), null);
  assert.equal(sanitizeUrl(""), null);
});

test("raw HTML is never interpreted; it stays literal text", () => {
  const blocks = parseMarkdown(
    "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>"
  );
  assert.deepEqual(
    blocks.map((b) => b.t),
    ["p", "p"]
  );
  const first = blocks[0];
  assert.ok(first && first.t === "p");
  assert.deepEqual(first.children, [
    { t: "text", text: "<script>alert(1)</script>" }
  ]);
  assert.equal(JSON.stringify(blocks).includes('"t":"html"'), false);
});

test("unsafe link targets lose the link but keep the label", () => {
  const inl = parseInline(
    "[click](javascript:alert(1)) and [ok](https://a.example/x)"
  );
  assert.deepEqual(links(inl), ["https://a.example/x"]);
  assert.equal(
    inl.some((n) => n.t === "text" && n.text === "click"),
    true
  );
  assert.deepEqual(links(parseInline("[x](data:text/html;base64,AAAA)")), []);
  assert.deepEqual(links(parseInline("[x](java\nscript:alert(1))")), []);
});

test("bare URLs autolink without trailing punctuation", () => {
  assert.deepEqual(links(parseInline("see https://example.com/a, ok")), [
    "https://example.com/a"
  ]);
  assert.deepEqual(links(parseInline("(https://example.com/a)")), [
    "https://example.com/a"
  ]);
  assert.deepEqual(
    links(parseInline("see https://en.wikipedia.org/wiki/A_(b).")),
    ["https://en.wikipedia.org/wiki/A_(b)"]
  );
});

test("inline: code, strong, em, strike, escapes", () => {
  assert.deepEqual(parseInline("`a*b*`"), [{ t: "code", text: "a*b*" }]);
  assert.deepEqual(parseInline("**b** *i* ~~d~~"), [
    { t: "strong", children: [{ t: "text", text: "b" }] },
    { t: "text", text: " " },
    { t: "em", children: [{ t: "text", text: "i" }] },
    { t: "text", text: " " },
    { t: "del", children: [{ t: "text", text: "d" }] }
  ]);
  assert.deepEqual(parseInline("snake_case_name"), [
    { t: "text", text: "snake_case_name" }
  ]);
  assert.deepEqual(parseInline("2 * 3 * 4"), [
    { t: "text", text: "2 * 3 * 4" }
  ]);
  assert.deepEqual(parseInline("\\*x\\*"), [{ t: "text", text: "*x*" }]);
  // unclosed markers (a streaming message) stay literal
  assert.deepEqual(parseInline("**half"), [{ t: "text", text: "**half" }]);
});

test("fenced code keeps its body verbatim, closed or not", () => {
  const closed = parseMarkdown("```ts\nconst a = *1*;\n```\nafter");
  assert.deepEqual(closed[0], {
    t: "code",
    lang: "ts",
    text: "const a = *1*;"
  });
  assert.equal(closed[1]?.t, "p");
  const open = parseMarkdown("```rust\nfn main() {");
  assert.deepEqual(open, [{ t: "code", lang: "rust", text: "fn main() {" }]);
  const inner = parseMarkdown("````\n```\nx\n```\n````");
  assert.deepEqual(inner, [{ t: "code", lang: "", text: "```\nx\n```" }]);
});

test("GFM table with alignment, escaped pipe and ragged rows", () => {
  const [tbl] = parseMarkdown(
    "| a | b | c |\n|:--|:-:|--:|\n| 1 | x \\| y | 3 |\n| only |"
  );
  assert.ok(tbl && tbl.t === "table");
  assert.deepEqual(tbl.align, ["left", "center", "right"]);
  assert.equal(tbl.head.length, 3);
  assert.equal(tbl.rows.length, 2);
  assert.deepEqual(tbl.rows[0]?.[1], [{ t: "text", text: "x | y" }]);
  assert.equal(tbl.rows[1]?.length, 3);
});

test("a lone pipe line is not a table", () => {
  assert.equal(parseMarkdown("a | b\nplain")[0]?.t, "p");
});

test("lists: bullets, ordered start, nesting, tasks", () => {
  const [ul] = parseMarkdown(
    "- one\n- two\n  - nested\n- [x] done\n- [ ] todo"
  );
  assert.ok(ul && ul.t === "list" && !ul.ordered);
  assert.equal(ul.items.length, 4);
  assert.equal(ul.items[1]?.children[1]?.t, "list");
  assert.equal(ul.items[2]?.checked, true);
  assert.equal(ul.items[3]?.checked, false);
  const [ol] = parseMarkdown("3. c\n4. d");
  assert.ok(ol && ol.t === "list" && ol.ordered);
  assert.equal(ol.start, 3);
  assert.equal(ol.items.length, 2);
});

test("headings, quotes, rules", () => {
  const b = parseMarkdown("# Title\n\n> quoted **x**\n\n---\n\ntext");
  assert.deepEqual(
    b.map((x) => x.t),
    ["heading", "quote", "hr", "p"]
  );
  assert.equal(b[0]?.t === "heading" && b[0].level, 1);
});

test("pathological nesting terminates", () => {
  const deep = "> ".repeat(200) + "x";
  assert.ok(parseMarkdown(deep).length === 1);
  assert.ok(parseInline("*".repeat(5000)).length > 0);
  assert.ok(parseInline("[".repeat(3000)).length > 0);
});
