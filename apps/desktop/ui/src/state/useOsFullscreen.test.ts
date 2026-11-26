import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

// The hook is a thin shell around `createFullscreenController` (rules tested in lib/osFullscreen.test.ts).
// The test runner has no DOM to mount it in, so what the shell must guarantee is checked in its source.
const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
const hook = read("./useOsFullscreen.ts");

test("the hook leaves fullscreen from an effect cleanup, which runs on unmount as well as on `enabled` turning false", () => {
  const effect = /useEffect\(\(\) => \{\s*if \(!enabled\) \{\s*return undefined;\s*\}\s*controller\.enable\(\);\s*return \(\) => controller\.disable\(\);\s*\}, \[enabled, controller\]\);/.exec(hook);
  assert.ok(effect, "enable on mount/open, disable in the cleanup");
  assert.doesNotMatch(hook, /enteredHere/, "the bookkeeping lives in the controller, not in the component");
  assert.doesNotMatch(hook, /\.then\(/, "no unguarded async result handling in the component");
});

test("one controller serves the whole life of the hook, and its buttons are stable", () => {
  assert.match(hook, /useMemo\(\(\) => createFullscreenController\(osFullscreenBackend, setFullscreen\), \[\]\)/);
  assert.match(hook, /toggle: controller\.toggle, exit: controller\.exit/);
});

test("fullscreen is published on <html> so the custom window controls step aside, and removed again", () => {
  assert.match(hook, /root\.dataset\.osFullscreen = "true"/);
  assert.match(hook, /delete root\.dataset\.osFullscreen/);
  const css = read("../components/WindowControls.module.css");
  assert.match(css, /:global\(:root\[data-os-fullscreen="true"\]\) \.strip \{\s*display: none;/);
  assert.match(css, /:global\(:root\[data-os-fullscreen="true"\]\) \{\s*--titlebar-h: var\(--space-0\);/);
});
