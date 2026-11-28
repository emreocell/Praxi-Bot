import assert from "node:assert/strict";
import { test } from "node:test";
import type { DaemonEvent, GlobalEnvelope } from "../api/types.ts";
import {
  applyEvent,
  EMPTY_EVENTS,
  type ElicitationItem,
  type EventsState,
  needsYou,
  recordElicitationOutcome,
  reconcile
} from "./timeline.ts";

const RUN = "run-1";
const CALL = "0b6f8f3e-1c2d-4e5f-8a9b-0c1d2e3f4a5b";
let gseq = 0;
function feed(state: EventsState, event: DaemonEvent, ts = 1000): EventsState {
  gseq += 1;
  const env: GlobalEnvelope = { v: 1, seq: gseq, ts_ms: ts, gseq, event };
  return applyEvent(state, env);
}

function requested(expires = 300): EventsState {
  gseq = 0;
  let s = feed(EMPTY_EVENTS, { type: "run_started", run_id: RUN, chat_id: "chat-1", bot_id: "bot-1" });
  s = feed(s, { type: "tool_call_requested", run_id: RUN, call_id: "t1", tool_name: "crm.create", input: {} });
  return feed(
    s,
    {
      type: "elicitation_requested",
      run_id: RUN,
      call_id: CALL,
      server: "acme-crm",
      message: "Need your e-mail",
      fields: [{ name: "email", title: null, description: null, type: "string", required: true, format: "email" }],
      expires_in_secs: expires
    },
    10_000
  );
}

const card = (s: EventsState) => s.runs[RUN].items.find((i) => i.kind === "elicitation") as ElicitationItem;

test("elicitation_requested adds its own card next to the tool card and is a pending decision", () => {
  const s = requested();
  assert.equal(s.runs[RUN].items.length, 2, "the MCP tool card stays: the request has its own id");
  const item = card(s);
  assert.equal(item.server, "acme-crm");
  assert.equal(item.expiresMs, 10_000 + 300_000);
  const need = needsYou(s, [], 20_000);
  assert.equal(need.length, 1);
  assert.equal(need[0].kind, "elicitation");
  assert.equal(need[0].elicitation?.callId, CALL);
  assert.equal(need[0].botId, "bot-1");
});

test("a replayed event does not add a second card", () => {
  const s = feed(requested(), {
    type: "elicitation_requested", run_id: RUN, call_id: CALL, server: "acme-crm", message: "x", fields: [], expires_in_secs: 300
  });
  assert.equal(s.runs[RUN].items.filter((i) => i.kind === "elicitation").length, 1);
});

test("expires_in_secs 0 means no limit; otherwise it stops being pinned at expiry", () => {
  const timed = requested();
  assert.equal(needsYou(timed, [], 309_999).length, 1);
  assert.equal(needsYou(timed, [], 310_000).length, 0);
  const open = requested(0);
  assert.equal(card(open).expiresMs, Number.POSITIVE_INFINITY);
  assert.equal(needsYou(open, [], 10 ** 13).length, 1);
});

test("the card closes when answered, declined, cancelled, when the run ends or the daemon forgets the run", () => {
  for (const outcome of ["accepted", "declined", "cancelled"] as const) {
    const s = recordElicitationOutcome(requested(), RUN, CALL, outcome);
    assert.equal(card(s).outcome, outcome);
    assert.equal(needsYou(s, [], 20_000).length, 0);
  }
  const finished = feed(requested(), { type: "run_finished", run_id: RUN, error: null });
  assert.equal(card(finished).closed, true);
  assert.equal(needsYou(finished, [], 20_000).length, 0);

  const orphan = reconcile(requested(), new Set(), new Set(), 20_000);
  assert.equal(card(orphan).closed, true);
});
