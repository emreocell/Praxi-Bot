import assert from "node:assert/strict";
import { test } from "node:test";
import { createTauriBridge, type BridgeRoute } from "./tauriBridge.ts";

function harness() {
  const handlers = new Map<string, ((e: { payload: unknown }) => void)[]>();
  let unlistened = 0;
  const listen = (
    event: string,
    handler: (e: { payload: unknown }) => void
  ): Promise<() => void> => {
    const list = handlers.get(event) ?? [];
    list.push(handler);
    handlers.set(event, list);
    return Promise.resolve(() => {
      unlistened++;
      const l = handlers.get(event) ?? [];
      handlers.set(
        event,
        l.filter((h) => h !== handler)
      );
    });
  };
  const published: unknown[] = [];
  const routes: BridgeRoute[] = [
    { wire: "control:state_changed", publish: (p) => published.push(p) }
  ];
  const emitWire = (event: string, payload: unknown) => {
    for (const h of [...(handlers.get(event) ?? [])]) h({ payload });
  };
  return {
    bridge: createTauriBridge(listen, routes),
    handlers,
    published,
    emitWire,
    unlistened: () => unlistened
  };
}

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

test("two subscribers share one Tauri listener: one publish per event", async () => {
  const h = harness();
  const a = h.bridge.acquire();
  const b = h.bridge.acquire();
  await tick();
  assert.equal(h.handlers.get("control:state_changed")?.length, 1);
  h.emitWire("control:state_changed", 1);
  assert.deepEqual(h.published, [1]);
  a();
  h.emitWire("control:state_changed", 2);
  assert.deepEqual(h.published, [1, 2]);
  b();
  assert.equal(h.unlistened(), 1);
  assert.equal(h.handlers.get("control:state_changed")?.length, 0);
});

test("release is idempotent and re-acquire installs a fresh listener", async () => {
  const h = harness();
  const a = h.bridge.acquire();
  await tick();
  a();
  a();
  assert.equal(h.bridge.refs(), 0);
  const b = h.bridge.acquire();
  await tick();
  h.emitWire("control:state_changed", "x");
  assert.deepEqual(h.published, ["x"]);
  b();
});

test("releasing before registration resolves does not leak the listener", async () => {
  const h = harness();
  const a = h.bridge.acquire();
  a();
  await tick();
  assert.equal(h.handlers.get("control:state_changed")?.length ?? 0, 0);
  h.emitWire("control:state_changed", 1);
  assert.deepEqual(h.published, []);
});
