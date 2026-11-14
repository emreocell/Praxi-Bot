import assert from "node:assert/strict";
import { test } from "node:test";
import type { InputEvent } from "../api/types.ts";
import {
  createStageController,
  focusLeftScope,
  type StageDeps
} from "./liveViewStage.ts";

type Props = Record<string, ((e: never) => void) | undefined>;

/** Bubble one synthetic event from the target (path[0]) up to the root. */
function dispatch(
  path: readonly Props[],
  name: string,
  base: Record<string, unknown> = {}
): { defaultPrevented: boolean } {
  let stopped = false;
  let defaultPrevented = false;
  const e = {
    ...base,
    preventDefault: () => {
      defaultPrevented = true;
    },
    stopPropagation: () => {
      stopped = true;
    }
  };
  for (const node of path) {
    node[name]?.(e as never);
    if (stopped) {
      break;
    }
  }
  return { defaultPrevented };
}

function harness(opts: { interactive?: boolean; captureTab?: boolean } = {}) {
  const events: InputEvent[] = [];
  const log = { resets: 0, exits: 0, composing: [] as boolean[] };
  const deps: StageDeps = {
    isInteractive: () => opts.interactive ?? true,
    captureTab: () => opts.captureTab ?? false,
    mapPointer: () => ({ x: 0.5, y: 0.25 }),
    stageSize: () => ({ w: 800, h: 600 }),
    enqueue: (ev) => void events.push(ev),
    resetInput: () => void (log.resets += 1),
    focusIme: () => undefined,
    markFocused: () => undefined,
    exitKeyboardFocus: () => void (log.exits += 1),
    setComposing: (c) => void log.composing.push(c),
    clearImeBuffer: () => undefined
  };
  const ctl = createStageController(() => deps);
  return { events, log, ctl };
}

const key = (code: string, k: string, extra: Record<string, unknown> = {}) => ({
  code,
  key: k,
  location: 0,
  repeat: false,
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  nativeEvent: { isComposing: false },
  ...extra
});

const pointer = (extra: Record<string, unknown> = {}) => ({
  button: 0,
  buttons: 1,
  clientX: 10,
  clientY: 10,
  pointerId: 1,
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  currentTarget: {
    setPointerCapture: () => undefined,
    hasPointerCapture: () => false,
    releasePointerCapture: () => undefined
  },
  ...extra
});

test("each physical key is forwarded exactly once although the IME textarea sits inside the stage", () => {
  const { events, ctl } = harness();
  const stage = ctl.stage as unknown as Props;
  const ime = ctl.ime as unknown as Props;
  // target = textarea, parent = stage div (the real DOM nesting)
  for (const ch of "hello") {
    const code = `Key${ch.toUpperCase()}`;
    dispatch([ime, stage], "onKeyDown", key(code, ch));
    dispatch([ime, stage], "onKeyUp", key(code, ch));
  }
  const downs = events.filter((e) => e.type === "key" && e.phase === "down");
  const ups = events.filter((e) => e.type === "key" && e.phase === "up");
  assert.equal(downs.length, 5, "5 key downs for 5 keys, not 10");
  assert.equal(ups.length, 5);
  assert.deepEqual(
    downs.map((e) => (e.type === "key" ? e.code : "")),
    ["KeyH", "KeyE", "KeyL", "KeyL", "KeyO"]
  );
});

test("the stage element carries no key handlers and a forwarded key stops bubbling", () => {
  const { events, ctl } = harness();
  const stage = ctl.stage as unknown as Props;
  assert.equal(stage.onKeyDown, undefined);
  assert.equal(stage.onKeyUp, undefined);
  // Even if some future wiring attached the same handler on the parent too:
  const ime = ctl.ime as unknown as Props;
  const parentCopy: Props = { onKeyDown: ime.onKeyDown, onKeyUp: ime.onKeyUp };
  dispatch([ime, parentCopy], "onKeyDown", key("KeyA", "a"));
  dispatch([ime, parentCopy], "onKeyUp", key("KeyA", "a"));
  assert.equal(events.length, 2);
});

test("clicking the local toolbar never becomes remote pointer input", () => {
  const { events, ctl } = harness();
  const stage = ctl.stage as unknown as Props;
  const toolbar = ctl.toolbar as unknown as Props;
  const button: Props = {};
  // target = Release focus button, parent = toolbar, grandparent = stage
  for (const name of [
    "onPointerDown",
    "onPointerMove",
    "onPointerUp",
    "onPointerCancel",
    "onMouseDown",
    "onWheel"
  ]) {
    dispatch([button, toolbar, stage], name, {
      ...pointer(),
      deltaX: 0,
      deltaY: 5,
      deltaMode: 0
    });
  }
  assert.deepEqual(events, [], "toolbar events stop before the stage handlers");

  // Control: the same events on the picture itself do reach the remote.
  const img: Props = {};
  dispatch([img, stage], "onPointerDown", pointer());
  dispatch([img, stage], "onPointerUp", pointer({ buttons: 0 }));
  assert.equal(events.length, 2);

  // Negative control: without the toolbar guard the click would leak (the bug).
  const leaking: Props = {};
  dispatch([button, leaking, stage], "onPointerDown", pointer());
  assert.equal(events.length, 3);
});

test("a cancelled pointer gesture resets input instead of inventing a left up", () => {
  const { events, log, ctl } = harness();
  const stage = ctl.stage as unknown as Props;
  dispatch([stage], "onPointerDown", pointer());
  assert.equal(events.length, 1);
  dispatch([stage], "onPointerCancel", pointer());
  assert.equal(events.length, 1, "no pointer up event");
  assert.equal(log.resets, 1);
});

test("nothing is forwarded while the stage is not interactive", () => {
  const { events, ctl } = harness({ interactive: false });
  const ime = ctl.ime as unknown as Props;
  const stage = ctl.stage as unknown as Props;
  dispatch([ime, stage], "onKeyDown", key("KeyA", "a"));
  dispatch([stage], "onPointerDown", pointer());
  assert.deepEqual(events, []);
});

test("Shift+Escape exits keyboard focus without forwarding Escape; Tab is not captured by default", () => {
  const { events, log, ctl } = harness();
  const ime = ctl.ime as unknown as Props;
  dispatch([ime], "onKeyDown", key("Escape", "Escape", { shiftKey: true }));
  assert.equal(log.exits, 1);
  assert.deepEqual(events, []);
  const tab = dispatch([ime], "onKeyDown", key("Tab", "Tab"));
  assert.deepEqual(events, []);
  assert.equal(tab.defaultPrevented, false, "Tab moves focus out of the stage");

  const captured = harness({ captureTab: true });
  dispatch(
    [captured.ctl.ime as unknown as Props],
    "onKeyDown",
    key("Tab", "Tab")
  );
  assert.equal(captured.events.length, 1);
});

test("Turkish keys travel as text exactly once through the real nesting (no key + text duplicate)", () => {
  const { events, ctl } = harness();
  const ime = ctl.ime as unknown as Props;
  const stage = ctl.stage as unknown as Props;
  const turkish: Array<[string, string]> = [
    ["Semicolon", "ş"],
    ["BracketLeft", "ğ"],
    ["BracketRight", "ü"],
    ["Quote", "i"],
    ["Comma", "ö"],
    ["Period", "ç"],
    ["KeyI", "ı"]
  ];
  for (const [code, k] of turkish) {
    dispatch([ime, stage], "onKeyDown", key(code, k));
    dispatch([ime, stage], "onKeyUp", key(code, k));
  }
  assert.deepEqual(
    events,
    turkish.map(([, k]) => ({ type: "text", text: k }))
  );
});

test("IME composition commits text once and emits no key events while composing", () => {
  const { events, log, ctl } = harness();
  const ime = ctl.ime as unknown as Props;
  ctl.ime.onCompositionStart();
  dispatch([ime], "onKeyDown", key("KeyS", "Process", {
    nativeEvent: { isComposing: true }
  }));
  ctl.ime.onCompositionEnd({ data: "ş" });
  assert.deepEqual(events, [{ type: "text", text: "ş" }]);
  assert.deepEqual(log.composing, [true, false]);
});

test("focus scope: moving focus onto a toolbar button inside the stage is not leaving it", () => {
  const inside = new Set(["textarea", "toolbarButton"]);
  const container = { contains: (n: unknown) => inside.has(n as string) };
  assert.equal(focusLeftScope("toolbarButton", container), false);
  assert.equal(focusLeftScope("textarea", container), false);
  assert.equal(focusLeftScope("chatInput", container), true);
  assert.equal(focusLeftScope(null, container), true);
});
