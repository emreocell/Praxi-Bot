import assert from "node:assert/strict";
import { test } from "node:test";
import {
  newChatIdFromText,
  parseRoute,
  type RouteState,
  routeChip
} from "./route.ts";

const state = (route: unknown, level = "K1"): RouteState => ({
  level,
  route: parseRoute(route),
  confidence: 0.92,
  latencyMs: 3,
  reason: "cache hit"
});

test("parseRoute reads each kind and tolerates junk", () => {
  assert.deepEqual(parseRoute({ kind: "command", name: "/stop", args: [] }), {
    kind: "command",
    name: "stop"
  });
  assert.equal(
    parseRoute({ kind: "workflow", name: "w", confidence: 0.9 }).kind,
    "workflow"
  );
  assert.deepEqual(parseRoute({ kind: "tool_hint", tools: ["a", 1, "b"] }), {
    kind: "tool_hint",
    tools: ["a", "b"]
  });
  assert.equal(parseRoute({ kind: "agent" }).kind, "agent");
  assert.equal(parseRoute({ kind: "future" }).kind, "unknown");
  assert.equal(parseRoute(null).kind, "unknown");
  assert.equal(parseRoute({ kind: "workflow" }).kind, "unknown");
});

test("chips: workflow, command, hints; plain Agent at K4 is silent", () => {
  assert.equal(
    routeChip(state({ kind: "workflow", name: "x" }), false)?.i18nKey,
    "chat.route.workflow"
  );
  assert.equal(
    routeChip(state({ kind: "command", name: "stop" }, "K0"), false)?.values
      .name,
    "stop"
  );
  assert.equal(
    routeChip(state({ kind: "tool_hint", tools: ["a"] }), false)?.i18nKey,
    "chat.route.toolHint"
  );
  assert.equal(routeChip(state({ kind: "agent" }, "K4"), false), null);
  assert.equal(
    routeChip(state({ kind: "agent" }, "K4"), true)?.i18nKey,
    "chat.route.agent"
  );
  assert.equal(
    routeChip(state({ kind: "agent" }, "K3"), false)?.values.level,
    "K3"
  );
  assert.equal(routeChip(state({ kind: "zzz" }), true), null);
});

test("newChatIdFromText finds the uuid of a /new turn", () => {
  const id = "0b9f4b8e-1d5c-4a8e-9f3a-3a1d2c4e5f60";
  assert.equal(newChatIdFromText(`Started a new chat: ${id}`), id);
  assert.equal(newChatIdFromText("hello"), null);
  assert.equal(newChatIdFromText("Started a new chat: nope"), null);
});
