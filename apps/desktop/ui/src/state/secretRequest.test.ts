import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import {
  applyEvent,
  EMPTY_EVENTS,
  type EventsState,
  needsYou,
  recordSecretOutcome,
  type SecretRequestItem
} from "./timeline.ts";

const RUN = "run-1";
let gseq = 0;
function feed(state: EventsState, event: DaemonEvent, ts = 1000): EventsState {
  gseq += 1;
  const env: GlobalEnvelope = { v: 1, seq: gseq, ts_ms: ts, gseq, event };
  return applyEvent(state, env);
}

function requested(): EventsState {
  gseq = 0;
  let s = feed(EMPTY_EVENTS, { type: "run_started", run_id: RUN, chat_id: "chat-1", bot_id: "bot-1" });
  s = feed(s, { type: "tool_call_requested", run_id: RUN, call_id: "s1", tool_name: "secret_request", input: {} });
  return feed(
    s,
    {
      type: "secret_requested",
      run_id: RUN,
      call_id: "s1",
      name: "login/example",
      purpose: "Sign in",
      fields: "login",
      replaces_existing: true,
      untrusted_content: true,
      expires_in_secs: 300
    },
    10_000
  );
}

test("secret_requested becomes one card with an expiry, and is a pending decision", () => {
  const s = requested();
  const items = s.runs[RUN].items;
  assert.equal(items.length, 1, "the secret_request tool card is replaced");
  const item = items[0] as SecretRequestItem;
  assert.equal(item.kind, "secret");
  assert.equal(item.expiresMs, 10_000 + 300_000);
  assert.equal(item.replacesExisting, true);
  assert.equal(item.untrustedContent, true);
  const need = needsYou(s, [], 20_000);
  assert.equal(need.length, 1);
  assert.equal(need[0].kind, "secret");
  assert.equal(need[0].botId, "bot-1");
});

test("an expired request is no longer pinned (its card is locked)", () => {
  const s = requested();
  assert.equal(needsYou(s, [], 309_999).length, 1);
  assert.equal(needsYou(s, [], 310_000).length, 0);
});

test("the card closes when it is answered, when the call completes and when the run ends", () => {
  const answered = recordSecretOutcome(requested(), RUN, "s1", "stored");
  assert.equal(needsYou(answered).length, 0);
  assert.equal((answered.runs[RUN].items[0] as SecretRequestItem).outcome, "stored");

  const completed = feed(requested(), { type: "tool_call_completed", run_id: RUN, call_id: "s1", error: null });
  assert.equal(needsYou(completed).length, 0);
  assert.equal((completed.runs[RUN].items[0] as SecretRequestItem).closed, true);

  const finished = feed(requested(), { type: "run_finished", run_id: RUN, error: null });
  assert.equal(needsYou(finished).length, 0);
});

test("the stored timeline item never holds a typed value, only what the event carried", () => {
  const item = requested().runs[RUN].items[0] as SecretRequestItem;
  assert.deepEqual(Object.keys(item).sort(), [
    "callId", "closed", "expiresMs", "fields", "key", "kind", "name", "purpose",
    "replacesExisting", "requestedMs", "untrustedContent"
  ].filter((k) => k !== "closed" || k in item).sort());
});
