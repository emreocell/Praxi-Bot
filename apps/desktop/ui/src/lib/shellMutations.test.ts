import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { MUTATIONS } from "./shellMutations.ts";

const sidebarCss = readFileSync(
  new URL("../../../../../packages/ui/src/components/Sidebar.module.css", import.meta.url),
  "utf8"
);

test("every mutation has a unique name and says what failure it expects", () => {
  const names = MUTATIONS.map((m) => m.name);
  assert.equal(new Set(names).size, names.length);
  for (const m of MUTATIONS) {
    assert.ok(m.expect instanceof RegExp, m.name);
    assert.ok(m.css || m.options, `${m.name} changes neither the CSS nor the options`);
  }
});

test("every CSS mutation applies to the real sidebar CSS and changes it", () => {
  for (const m of MUTATIONS) {
    if (!m.css) continue;
    const out = m.css(sidebarCss);
    assert.notEqual(out, sidebarCss, m.name);
  }
});

test("a CSS mutation fails loudly when its target is gone", () => {
  const withTarget = MUTATIONS.filter((m) => m.css && /window-resize|inner width 0px|open panel width/.test(m.name));
  assert.ok(withTarget.length >= 3);
  for (const m of withTarget) {
    assert.throws(() => m.css?.(".unrelated { color: red; }"), /mutation target not found/, m.name);
  }
});

test("the policy mutation carries its option", () => {
  const policy = MUTATIONS.filter((m) => m.options);
  assert.deepEqual(policy.map((m) => m.options), [{ railPolicy: "immediate" }]);
});
