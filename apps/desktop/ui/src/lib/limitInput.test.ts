import assert from "node:assert/strict";
import { test } from "node:test";
import { parseCountText, parseUsdText } from "./limitInput.ts";

test("money: digits with at most two decimals, comma or dot", () => {
  assert.equal(parseUsdText("12"), 12);
  assert.equal(parseUsdText(" 7,5 "), 7.5);
  assert.equal(parseUsdText("0.05"), 0.05);
  assert.equal(parseUsdText("1000"), 1000);
});

test("money: a thousands separator is rejected, not read as a decimal point", () => {
  for (const text of ["1.000", "1,000", "2.500", "1.000.000", "1,000,000", "1.234,56"]) {
    assert.equal(parseUsdText(text), null, text);
  }
});

test("money: signs, exponents, hex and junk are rejected", () => {
  for (const text of ["", " ", "-5", "+5", "1e3", "0x10", "abc", "5$", "1.", ".5", "1,2,3", "1.005"]) {
    assert.equal(parseUsdText(text), null, JSON.stringify(text));
  }
});

test("count: digits only", () => {
  assert.equal(parseCountText("2000000"), 2_000_000);
  assert.equal(parseCountText(" 42 "), 42);
  for (const text of ["", "10.000", "10,000", "1.5", "-1", "1e3", "0x10", "1 000", "abc"]) {
    assert.equal(parseCountText(text), null, JSON.stringify(text));
  }
});
