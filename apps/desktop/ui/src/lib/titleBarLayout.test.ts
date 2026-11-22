import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

/* The title-bar strip contract, checked on the shipped sources (no DOM in node --test):
   the panels' container starts below the strip, the strip is the drag region and carries the
   controls, and the controls cannot be covered because they live inside it. */
const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const layoutCss = read("../../../../../packages/ui/src/components/Sidebar.module.css");
const stripCss = read("../components/WindowControls.module.css");
const stripTsx = read("../components/WindowControls.tsx");

test("custom chrome: the layout content box (and so every panel) starts below the reserved strip", () => {
  const rule = /:root\[data-chrome="custom"\]\) \.layout \{([^}]*)\}/.exec(layoutCss)?.[1] ?? "";
  assert.match(rule, /padding-top:\s*var\(--titlebar-h\)/);
  assert.match(/\.layout \{([^}]*)\}/.exec(layoutCss)?.[1] ?? "", /height:\s*100vh/);
});

test("the strip spans the window, is the drag region and holds the controls", () => {
  const strip = /\.strip \{([^}]*)\}/.exec(stripCss)?.[1] ?? "";
  assert.match(strip, /position:\s*fixed/);
  assert.match(strip, /left:\s*0/);
  assert.match(strip, /right:\s*0/);
  assert.match(strip, /height:\s*var\(--titlebar-h\)/);
  assert.match(stripTsx, /className=\{styles\.strip\} data-tauri-drag-region/);
  assert.ok(stripTsx.indexOf("styles.strip") < stripTsx.indexOf("styles.controls"));
});

test("controls stay above the panels (z-index) and the panel container stays below the strip", () => {
  const z = Number(/\.strip \{[^}]*z-index:\s*(\d+)/.exec(stripCss)?.[1]);
  assert.ok(z >= 1000);
});
