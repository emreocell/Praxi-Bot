import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { evalLength, parseRules, readShellContract } from "./shellContract.ts";
import { FLOAT_GAP, FLOAT_PAD } from "./shellLayout.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const sources = {
  sidebarTsx: read("../../../../../packages/ui/src/components/Sidebar.tsx"),
  sidebarCss: read("../../../../../packages/ui/src/components/Sidebar.module.css"),
  tokensCss: read("../../../../../packages/ui/tokens.css"),
  globalCss: read("../global.css"),
  themeTs: read("../../../../../packages/ui/src/theme.ts")
};
const expect = { floatPad: FLOAT_PAD, floatGap: FLOAT_GAP };

test("parseRules splits selector lists, keeps declaration order and ignores comments", () => {
  const rules = parseRules("/* x { a: b } */ .a, .b { width: 1px; width: 2px; color: red }");
  assert.deepEqual(
    rules.map((r) => r.selector),
    [".a", ".b"]
  );
  assert.deepEqual(rules[0].list, [
    ["width", "1px"],
    ["width", "2px"],
    ["color", "red"]
  ]);
  assert.equal(rules[0].decls.get("width"), "2px");
});

test("evalLength handles precedence, parentheses, division and unsupported values", () => {
  const vars = new Map<string, string>();
  assert.equal(evalLength("calc(2px + 3px * 4)", vars), 14);
  assert.equal(evalLength("calc((2px + 3px) * 4)", vars), 20);
  assert.equal(evalLength("calc(10px / 4)", vars), 2.5);
  assert.equal(evalLength("calc(var(--a) + 1px)", new Map([["--a", "5px"]])), 6);
  assert.equal(evalLength("calc(var(--local) * 2)", vars, new Map([["--local", "4px"]])), 8);
  assert.equal(evalLength("auto", vars), undefined);
  assert.equal(evalLength("var(--nope)", vars), undefined);
  assert.equal(evalLength("calc(1px / 0)", vars), undefined);
});

test("the shipped sources satisfy the contract and yield a usable model", () => {
  const { model, failures } = readShellContract(sources, expect);
  assert.deepEqual(failures, []);
  assert.equal(model.floatPad, FLOAT_PAD);
  assert.equal(model.floatGap, FLOAT_GAP);
  assert.ok(model.slideMs > 0);
  assert.equal(model.states.plainOpen.width(320), 320);
  assert.ok(model.innerWidth(320, "plainOpen") > 0);
  assert.ok(model.innerWidth(320, "plainOpen") <= 320);
});

test("a conditional at-rule around a contracted selector is rejected", () => {
  const css = `${sources.sidebarCss}\n@media (min-width: 1px) { .panelInner { width: 0px; } }\n`;
  const { failures } = readShellContract({ ...sources, sidebarCss: css }, expect);
  assert.ok(failures.some((f) => /at-rule/.test(f)));
});

test("a different float padding than the one the app exports is reported", () => {
  const { failures } = readShellContract(sources, { floatPad: FLOAT_PAD + 1, floatGap: FLOAT_GAP });
  assert.ok(failures.length > 0);
});
