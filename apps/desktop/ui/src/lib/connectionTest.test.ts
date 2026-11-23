import assert from "node:assert/strict";
import { test } from "node:test";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  CONNECTION_OUTCOMES,
  connectionDocsUrl,
  detailsText,
  normalizeOutcome
} from "./connectionTest.ts";

const GUIDE = fileURLToPath(new URL("../../../../../docs/guide/", import.meta.url));

test("an outcome the UI does not know is shown as unknown", () => {
  assert.equal(normalizeOutcome("bad_key"), "bad_key");
  assert.equal(normalizeOutcome("from_the_future"), "unknown");
});

test("every failing outcome links to a real section of the guide page in each language", () => {
  for (const [lang, dir] of [["en", ""], ["tr", "tr/"], ["de", "de/"]] as const) {
    const page = `${GUIDE}${dir}connection-test.md`;
    assert.ok(existsSync(page), page);
    const text = readFileSync(page, "utf8");
    for (const outcome of CONNECTION_OUTCOMES.filter((o) => o !== "ok")) {
      const url = connectionDocsUrl(lang, outcome);
      const anchor = url.split("#")[1];
      assert.ok(url.includes(`${dir}connection-test.md#`), url);
      assert.ok(text.includes(`<a id="${anchor}"></a>`), `${lang}: ${anchor}`);
    }
  }
});

test("details lead with the HTTP status and the service's own code", () => {
  assert.equal(
    detailsText({ target: "provider", outcome: "bad_key", http_status: 401, provider_code: "invalid_api_key", details: "nope" }),
    "HTTP 401\ncode: invalid_api_key\nnope"
  );
  assert.equal(detailsText({ target: "mcp", outcome: "ok", details: "" }), "");
});
