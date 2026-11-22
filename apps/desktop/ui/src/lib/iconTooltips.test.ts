import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import ts from "typescript";

/**
 * An icon-only button (an aria-label and nothing but icons inside) must also carry a `title`, so
 * a mouse user who hovers it sees what it does. Run with FIX_TOOLTIPS=1 to add the missing ones.
 */
const root = fileURLToPath(new URL("../", import.meta.url));
const TAGS = new Set(["button", "Button"]);
/** Owned by another worker at the time of writing (schedules view); its calendar arrows get their tooltips there. */
const SKIP = new Set(["views/SchedulesView.tsx"]);

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      return name === "dev" ? [] : files(p);
    }
    return p.endsWith(".tsx") ? [p] : [];
  });
}

function attr(node: ts.JsxOpeningLikeElement, name: string): ts.JsxAttribute | undefined {
  return node.attributes.properties.find((a): a is ts.JsxAttribute => ts.isJsxAttribute(a) && a.name.getText() === name);
}

/** True when the node (or anything inside it) can show words: text, or an expression that is not an icon element. */
function showsText(c: ts.Node): boolean {
  if (ts.isJsxText(c)) {
    return c.text.trim() !== "";
  }
  if (ts.isJsxExpression(c)) {
    const e = c.expression?.getText() ?? "";
    return e !== "" && !/^</.test(e) && !/^(?:\w+\s*&&\s*)?</.test(e) && !/^\w+\s*\?\s*</.test(e) && !/^\w+\s*\?\s*\(?\s*</.test(e);
  }
  if (ts.isJsxElement(c)) {
    return c.children.some(showsText);
  }
  return false;
}

/** Positions (and the label text) of icon-only buttons that have no title. */
function missing(path: string): Array<{ pos: number; label: string }> {
  const text = readFileSync(path, "utf8");
  const sf = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const out: Array<{ pos: number; label: string }> = [];
  const visit = (n: ts.Node) => {
    if (ts.isJsxElement(n) && TAGS.has(n.openingElement.tagName.getText())) {
      const open = n.openingElement;
      const label = attr(open, "aria-label");
      const hasText = n.children.some(showsText);
      if (label?.initializer && !attr(open, "title") && !hasText) {
        out.push({ pos: label.getEnd(), label: label.initializer.getText().replace(/^\{|\}$/g, "") });
      }
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return out;
}

test("icon-only buttons show a tooltip", () => {
  const fix = process.env.FIX_TOOLTIPS === "1";
  const problems: string[] = [];
  for (const path of files(root)) {
    if (SKIP.has(path.slice(root.length).replaceAll("\\", "/"))) {
      continue;
    }
    const found = missing(path);
    if (found.length === 0) {
      continue;
    }
    if (fix) {
      let text = readFileSync(path, "utf8");
      for (const f of [...found].sort((a, b) => b.pos - a.pos)) {
        text = `${text.slice(0, f.pos)} title={${f.label}}${text.slice(f.pos)}`;
      }
      writeFileSync(path, text);
    } else {
      problems.push(...found.map((f) => `${path.slice(root.length)}: ${f.label}`));
    }
  }
  assert.deepEqual(problems, []);
});
