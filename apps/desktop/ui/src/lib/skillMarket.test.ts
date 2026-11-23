import assert from "node:assert/strict";
import { test } from "node:test";
import {
  NOTHING_CONFIRMED,
  buildInstallBody,
  canConfirm,
  checkIndexUrl,
  filterByQuery,
  missingConfirmations,
  needsKey,
  sortedNeeds,
  type MarketPreviewLike
} from "./skillMarket.ts";

const base: MarketPreviewLike = {
  risk: "low",
  needs: [],
  requires_risk_acceptance: false,
  requires_access_confirmation: false,
  fingerprint: "abc",
  added_needs: [],
  removed_needs: [],
  installed: null
};

test("a skill above Low cannot be installed until the owner accepts the risk", () => {
  const risky = { ...base, risk: "high", requires_risk_acceptance: true };
  assert.deepEqual(missingConfirmations(risky, NOTHING_CONFIRMED), ["risk"]);
  assert.equal(canConfirm(risky, NOTHING_CONFIRMED), false);
  assert.equal(canConfirm(risky, { acceptRisk: true, acceptMore: false }), true);
  assert.equal(canConfirm(base, NOTHING_CONFIRMED), true);
});

test("an update that asks for more needs its own second confirmation", () => {
  const grown = { ...base, requires_access_confirmation: true, added_needs: ["network"] };
  assert.deepEqual(missingConfirmations(grown, { acceptRisk: true, acceptMore: false }), ["access"]);
  assert.equal(canConfirm(grown, { acceptRisk: false, acceptMore: true }), true);
});

test("the install request never carries a tick the dialog did not ask for", () => {
  const both = { acceptRisk: true, acceptMore: true };
  const body = buildInstallBody(base, { name: "tidy", indexUrl: null, update: false, confirm: both });
  assert.equal(body.accept_risk, false);
  assert.equal(body.accept_more_access, false);
  assert.equal("index_url" in body, false);
  assert.equal(body.accept, "abc");
  const risky = buildInstallBody(
    { ...base, requires_risk_acceptance: true },
    { name: "tidy", indexUrl: "https://example.com/i.json", update: true, confirm: both }
  );
  assert.equal(risky.accept_risk, true);
  assert.equal(risky.index_url, "https://example.com/i.json");
});

test("custom list addresses must be https on a named host", () => {
  assert.equal(checkIndexUrl(""), "empty");
  assert.equal(checkIndexUrl("http://example.com/i.json"), "not_https");
  assert.equal(checkIndexUrl("https://127.0.0.1/i.json"), "bad_host");
  assert.equal(checkIndexUrl("https://localhost/i.json"), "bad_host");
  assert.equal(checkIndexUrl("https://user@example.com/i.json"), "bad_host");
  assert.equal(checkIndexUrl("  https://example.com/i.json "), "ok");
});

test("capabilities are listed most serious first and unknown ones get a generic line", () => {
  assert.deepEqual(sortedNeeds(["large", "network", "scripts", "network"]), ["scripts", "network", "large"]);
  assert.equal(needsKey("scripts"), "skillMarket.needs.scripts");
  assert.equal(needsKey("from-the-future"), "skillMarket.needs.other");
});

test("search matches name, description and tags", () => {
  const list = [
    { name: "tidy", description: "Tidies files", tags: ["home"] },
    { name: "prices", description: "Watch prices", tags: ["shopping"] }
  ];
  assert.deepEqual(filterByQuery(list, "SHOP").map((s) => s.name), ["prices"]);
  assert.equal(filterByQuery(list, "").length, 2);
});
