import assert from "node:assert/strict";
import test from "node:test";
import { signedMaterial, webhookAddress } from "./incomingWebhooks.ts";

test("the address stays on the daemon and the signature binds the timestamp to the body", () => {
  assert.equal(
    webhookAddress("http://127.0.0.1:9/", "abc"),
    "http://127.0.0.1:9/v1/incoming-webhooks/abc"
  );
  assert.equal(signedMaterial("100", "call:danger_run {}"), "100.call:danger_run {}");
});
