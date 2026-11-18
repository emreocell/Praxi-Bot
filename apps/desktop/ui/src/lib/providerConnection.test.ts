import assert from "node:assert/strict";
import { test } from "node:test";
import { PROVIDERS_LOCAL_ONLY, canManageProviders } from "./providerConnection.ts";

const conn = (base_url: string, started_by_app = true) => ({ base_url, started_by_app });

test("the app's own loopback daemon can manage providers", () => {
  for (const url of ["http://127.0.0.1:8080", "http://localhost:9000", "http://[::1]:9000"]) {
    assert.equal(canManageProviders({ connection: conn(url), baseUrl: url }), true, url);
  }
});

test("a trailing slash on either side does not matter", () => {
  assert.equal(
    canManageProviders({ connection: conn("http://127.0.0.1:8080/"), baseUrl: "http://127.0.0.1:8080" }),
    true
  );
  assert.equal(
    canManageProviders({ connection: conn("http://127.0.0.1:8080"), baseUrl: "http://127.0.0.1:8080/" }),
    true
  );
});

test("an attached daemon, a missing connection or a missing base url cannot", () => {
  const url = "http://127.0.0.1:8080";
  assert.equal(canManageProviders({ connection: conn(url, false), baseUrl: url }), false);
  assert.equal(canManageProviders({ connection: null, baseUrl: url }), false);
  assert.equal(canManageProviders({ connection: conn(url), baseUrl: undefined }), false);
  assert.equal(canManageProviders({ connection: conn(url), baseUrl: "" }), false);
});

test("remote hosts, https and a client pointing elsewhere cannot", () => {
  assert.equal(
    canManageProviders({ connection: conn("http://example.com"), baseUrl: "http://example.com" }),
    false
  );
  assert.equal(
    canManageProviders({ connection: conn("https://127.0.0.1:8080"), baseUrl: "https://127.0.0.1:8080" }),
    false
  );
  assert.equal(
    canManageProviders({ connection: conn("http://127.0.0.1:8080"), baseUrl: "http://127.0.0.1:9999" }),
    false
  );
});

test("an unparsable connection url is refused, and the code constant is stable", () => {
  assert.equal(canManageProviders({ connection: conn("not a url"), baseUrl: "not a url" }), false);
  assert.equal(PROVIDERS_LOCAL_ONLY, "providers_require_app_daemon");
});
