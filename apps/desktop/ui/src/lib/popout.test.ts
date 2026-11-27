import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildPopoutSearchParams,
  chatPopoutLabel,
  distanceFromRect,
  isAlreadyDecidedError,
  isDragOutsideWindow,
  isPopoutLabel,
  isValidPopoutId,
  livePopoutLabel,
  popoutKindFromLabel,
  resolveWindowRoute,
  shouldPopOutOnDrag
} from "./popoutModel.ts";

test("popout IDs and labels validate cleanly", () => {
  assert.equal(isValidPopoutId("chat-123_abc"), true);
  assert.equal(isValidPopoutId(""), false);
  assert.equal(isValidPopoutId("../evil"), false);
  assert.equal(isValidPopoutId("a/b"), false);

  assert.equal(chatPopoutLabel("abc-123"), "chat-abc-123");
  assert.equal(chatPopoutLabel("bad/id"), null);
  assert.equal(livePopoutLabel("home"), "live-home");
  assert.equal(livePopoutLabel("local"), "live-local");

  assert.equal(popoutKindFromLabel("chat-abc-123"), "chat");
  assert.equal(popoutKindFromLabel("live-home"), "live");
  assert.equal(popoutKindFromLabel("live-local"), "live");
  assert.equal(popoutKindFromLabel("main"), null);
  assert.equal(popoutKindFromLabel("chat-../bad"), null);
  assert.equal(isPopoutLabel("chat-abc"), true);
  assert.equal(isPopoutLabel("widget"), false);
});

test("resolveWindowRoute supports chatId, id, live params, pinned, and falls back on bad IDs", () => {
  assert.deepEqual(resolveWindowRoute("?window=chat&chatId=c1&pinned=1&computerId=host_1"), {
    kind: "chat",
    chatId: "c1",
    computerId: "host_1",
    pinned: true
  });
  assert.deepEqual(resolveWindowRoute("?mock=1&window=chat&id=c2"), {
    kind: "chat",
    chatId: "c2",
    computerId: undefined,
    pinned: false
  });
  assert.deepEqual(resolveWindowRoute("", "", "chat-c3"), {
    kind: "chat",
    chatId: "c3",
    computerId: undefined,
    pinned: false
  });
  assert.deepEqual(resolveWindowRoute("?window=chat&chatId=../bad"), { kind: "main" });

  assert.deepEqual(
    resolveWindowRoute("?window=live&source=local&botId=b1&targetId=t1&pinned=true"),
    {
      kind: "live",
      source: "local",
      botId: "b1",
      targetId: "t1",
      computerId: undefined,
      pinned: true
    }
  );
  assert.deepEqual(resolveWindowRoute("", "", "live-home"), {
    kind: "live",
    source: "home",
    botId: undefined,
    targetId: undefined,
    computerId: undefined,
    pinned: false
  });
});

test("buildPopoutSearchParams encodes chat and live routes cleanly", () => {
  assert.equal(
    buildPopoutSearchParams({ kind: "chat", chatId: "chat_1", computerId: "h1" }, true),
    "?mock=1&window=chat&chatId=chat_1&computerId=h1"
  );
  assert.equal(
    buildPopoutSearchParams({ kind: "live", liveSource: "local", botId: "b1", targetId: "t1" }),
    "?window=live&source=local&botId=b1&targetId=t1"
  );
});

test("drag-out detection requires real drag and triggers outside viewport or far from source", () => {
  const viewport = { width: 1200, height: 800 };
  const sourceRect = { left: 100, top: 100, right: 320, bottom: 140 };

  assert.equal(isDragOutsideWindow({ x: -12, y: 200 }, viewport), true);
  assert.equal(isDragOutsideWindow({ x: 1220, y: 200 }, viewport), true);
  assert.equal(isDragOutsideWindow({ x: 600, y: 400 }, viewport), false);

  // Small click jitter must never pop out
  assert.equal(
    shouldPopOutOnDrag({ start: { x: 120, y: 120 }, current: { x: 123, y: 121 }, viewport, sourceRect }),
    false
  );
  // Dragging within/near source row (e.g. clicking inside list) stays inside
  assert.equal(
    shouldPopOutOnDrag({ start: { x: 120, y: 120 }, current: { x: 240, y: 125 }, viewport, sourceRect }),
    false
  );
  // A native mouseleave is delivered at the border itself, before coordinates go negative.
  assert.equal(shouldPopOutOnDrag({ start: { x: 120, y: 120 }, current: { x: 0, y: 120 }, viewport, sourceRect, leftWindow: true }), true);
  assert.equal(shouldPopOutOnDrag({ start: { x: 1, y: 120 }, current: { x: 0, y: 120 }, viewport, sourceRect, leftWindow: true }), false);
  // Dragging outside window edge pops out
  assert.equal(
    shouldPopOutOnDrag({ start: { x: 120, y: 120 }, current: { x: -15, y: 120 }, viewport, sourceRect }),
    true
  );
  // Dragging far away from source card pops out
  assert.equal(
    shouldPopOutOnDrag({ start: { x: 200, y: 120 }, current: { x: 500, y: 250 }, viewport, sourceRect }),
    true
  );
});

test("distanceFromRect is zero inside the rectangle and grows outside", () => {
  const rect = { left: 10, top: 20, right: 110, bottom: 80 };
  assert.equal(distanceFromRect({ x: 50, y: 40 }, rect), 0);
  assert.equal(distanceFromRect({ x: 0, y: 40 }, rect), 10);
  assert.equal(distanceFromRect({ x: 50, y: 5 }, rect), 15);
  assert.ok(distanceFromRect({ x: 200, y: 200 }, rect) > 90);
});

test("isAlreadyDecidedError identifies cross-window decision conflicts without matching unrelated errors", () => {
  assert.equal(
    isAlreadyDecidedError({ status: 404, code: "not_found", message: "pending approval for call c1 not found" }),
    true
  );
  assert.equal(
    isAlreadyDecidedError({ status: 409, code: "conflict", message: "already decided" }),
    true
  );
  assert.equal(
    isAlreadyDecidedError({ status: 410, code: "gone", message: "expired" }),
    true
  );
  assert.equal(
    isAlreadyDecidedError({ status: 500, code: "internal", message: "database locked" }),
    false
  );
  assert.equal(
    isAlreadyDecidedError({ status: 404, code: "not_found", message: "no route for POST /foo" }),
    false
  );
  assert.equal(isAlreadyDecidedError({ status: 409, code: "conflict", message: "scope not allowed" }), false);
  assert.equal(isAlreadyDecidedError({ status: 404, code: "not_found", message: "bot not found" }), false);
  assert.equal(isAlreadyDecidedError(new Error("network timeout")), false);
});
