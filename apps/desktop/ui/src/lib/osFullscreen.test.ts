import assert from "node:assert/strict";
import { test } from "node:test";
import { emit } from "@tauri-apps/api/event";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import {
  createFullscreenController,
  isOsFullscreen,
  setOsFullscreen,
  watchOsFullscreen,
  type FullscreenBackend
} from "./osFullscreen.ts";

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const settle = async () => {
  for (let i = 0; i < 6; i++) await tick();
};

type Listener = () => void;

/** A page with a window and a document that record their listeners; restores the globals afterwards. */
async function withPage<T>(
  setup: { tauri: boolean; fullscreenElement?: object | null },
  run: (page: {
    fire: (target: "window" | "document", type: string) => void;
    listeners: () => number;
    doc: Record<string, unknown>;
  }) => Promise<T>
): Promise<T> {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const handlers = new Map<string, Set<Listener>>();
  const target = (name: string) => ({
    addEventListener: (type: string, fn: Listener) => {
      const key = `${name}:${type}`;
      handlers.set(key, (handlers.get(key) ?? new Set()).add(fn));
    },
    removeEventListener: (type: string, fn: Listener) => void handlers.get(`${name}:${type}`)?.delete(fn)
  });
  const doc: Record<string, unknown> = {
    ...target("document"),
    fullscreenElement: setup.fullscreenElement ?? null,
    documentElement: {
      requestFullscreen: async () => {
        doc.fullscreenElement = {};
      }
    },
    exitFullscreen: async () => {
      doc.fullscreenElement = null;
    }
  };
  Object.defineProperty(globalThis, "window", {
    value: { ...target("window"), crypto: globalThis.crypto },
    configurable: true,
    writable: true
  });
  Object.defineProperty(globalThis, "document", { value: doc, configurable: true, writable: true });
  if (setup.tauri) mockWindows("main");
  try {
    return await run({
      fire: (name, type) => handlers.get(`${name}:${type}`)?.forEach((fn) => fn()),
      listeners: () => [...handlers.values()].reduce((n, set) => n + set.size, 0),
      doc
    });
  } finally {
    clearMocks();
    for (const [key, previous] of [["window", previousWindow], ["document", previousDocument]] as const) {
      if (previous) Object.defineProperty(globalThis, key, previous);
      else Reflect.deleteProperty(globalThis, key);
    }
  }
}

/** Installs the IPC mock of a Tauri window whose fullscreen state the test can read and change. */
function tauriWindow(initial = false) {
  const state = { on: initial, reads: 0, writes: [] as boolean[], refuse: false, gate: null as Promise<void> | null };
  mockIPC(
    async (command, payload) => {
      if (command === "plugin:window|is_fullscreen") {
        state.reads += 1;
        const value = state.on;
        if (state.gate) await state.gate;
        return value;
      }
      if (command === "plugin:window|set_fullscreen") {
        state.writes.push((payload as { value: boolean }).value);
        if (state.refuse) throw new Error("not allowed");
        state.on = (payload as { value: boolean }).value;
      }
      return undefined;
    },
    { shouldMockEvents: true }
  );
  return state;
}

test("in the desktop shell the window itself is asked, and a refusal leaves the state as it is", async () => {
  await withPage({ tauri: true }, async () => {
    const win = tauriWindow(false);
    assert.equal(await isOsFullscreen(), false);
    assert.equal(await setOsFullscreen(true), true);
    assert.deepEqual(win.writes, [true]);
    win.refuse = true;
    assert.equal(await setOsFullscreen(false), true, "refused: still fullscreen, reported as it is");
    win.refuse = false;
    assert.equal(await setOsFullscreen(false), false);
  });
});

test("in a plain browser the Fullscreen API is used", async () => {
  await withPage({ tauri: false }, async ({ doc }) => {
    assert.equal(await isOsFullscreen(), false);
    assert.equal(await setOsFullscreen(true), true);
    assert.notEqual(doc.fullscreenElement, null);
    assert.equal(await setOsFullscreen(false), false);
    assert.equal(doc.fullscreenElement, null);
  });
});

test("the watcher follows the shell's own resize event, which the page may never see", async () => {
  await withPage({ tauri: true }, async () => {
    const win = tauriWindow(false);
    const seen: boolean[] = [];
    const stop = watchOsFullscreen((on) => seen.push(on));
    await settle();
    win.on = true; // native fullscreen: no DOM resize / fullscreenchange fires, only the window event
    await emit("tauri://resize", { width: 1920, height: 1080 });
    await settle();
    assert.deepEqual(seen, [true]);
    win.on = false;
    await emit("tauri://resize", { width: 1920, height: 1080 });
    await settle();
    assert.deepEqual(seen, [true, false]);
    stop();
  });
});

test("the watcher also follows the page's resize and fullscreenchange (browser, F11)", async () => {
  await withPage({ tauri: false }, async ({ fire, doc }) => {
    const seen: boolean[] = [];
    const stop = watchOsFullscreen((on) => seen.push(on));
    doc.fullscreenElement = {};
    fire("document", "fullscreenchange");
    await settle();
    doc.fullscreenElement = null;
    fire("window", "resize");
    await settle();
    assert.deepEqual(seen, [true, false]);
    stop();
  });
});

test("a burst of events runs one check at a time and reports only real changes, the last state winning", async () => {
  await withPage({ tauri: true }, async ({ fire }) => {
    const win = tauriWindow(false);
    let release: () => void = () => {};
    win.gate = new Promise<void>((resolve) => (release = resolve));
    const seen: boolean[] = [];
    const stop = watchOsFullscreen((on) => seen.push(on));
    for (let i = 0; i < 20; i++) fire("window", "resize"); // a drag of the window edge
    await settle();
    assert.equal(win.reads, 1, "the check in flight is not started twice");
    win.on = true;
    win.gate = null;
    release();
    await settle();
    assert.equal(win.reads, 2, "the events that arrived meanwhile cost one more check, not twenty");
    assert.deepEqual(seen, [false, true], "the state after the burst is the one reported last");
    fire("window", "resize");
    await settle();
    assert.deepEqual(seen, [false, true], "no change, no report");
    stop();
  });
});

test("stopping the watcher removes the page listeners and ignores answers still on their way", async () => {
  await withPage({ tauri: false }, async ({ fire, listeners, doc }) => {
    const seen: boolean[] = [];
    const stop = watchOsFullscreen((on) => seen.push(on));
    assert.ok(listeners() >= 2);
    doc.fullscreenElement = {};
    fire("window", "resize");
    stop();
    await settle();
    assert.deepEqual(seen, [], "the answer that was already on its way is dropped");
    assert.equal(listeners(), 0);
    stop();
  });
});

// ── The controller: the live view's fullscreen rules, with a fake window ──

function fakeBackend(initial = false) {
  const log = { writes: [] as boolean[], watchers: 0, stopped: 0 };
  const state = { on: initial, refuse: false, delay: null as Promise<void> | null };
  let watcher: ((on: boolean) => void) | null = null;
  const backend: FullscreenBackend = {
    read: async () => state.on,
    set: async (on) => {
      log.writes.push(on);
      if (state.delay) await state.delay;
      if (!state.refuse) state.on = on;
      return state.on;
    },
    watch: (cb) => {
      log.watchers += 1;
      watcher = cb;
      return () => {
        log.stopped += 1;
        watcher = null;
      };
    }
  };
  return {
    backend,
    log,
    state,
    /** The window changes by itself (F11, the OS, Esc). */
    external: (on: boolean) => {
      state.on = on;
      watcher?.(on);
    },
    watching: () => watcher !== null
  };
}

function controllerWith(fake: ReturnType<typeof fakeBackend>) {
  const seen: boolean[] = [];
  const controller = createFullscreenController(fake.backend, (on) => seen.push(on));
  return { controller, seen };
}

test("the view's own fullscreen is undone when the view closes or unmounts", async () => {
  const fake = fakeBackend(false);
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  await settle();
  controller.toggle();
  await settle();
  assert.deepEqual(fake.log.writes, [true]);
  assert.equal(seen.at(-1), true);
  controller.disable(); // the effect cleanup: `enabled` turned false, or the component unmounted
  await settle();
  assert.deepEqual(fake.log.writes, [true, false], "fullscreen is left");
  assert.equal(fake.state.on, false);
  assert.equal(seen.at(-1), false);
  assert.equal(fake.log.stopped, 1, "the watcher is stopped too");
});

test("a fullscreen the owner already had is not taken away when the view closes", async () => {
  const fake = fakeBackend(true);
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  await settle();
  assert.equal(seen.at(-1), true);
  controller.disable();
  await settle();
  assert.deepEqual(fake.log.writes, []);
  assert.equal(fake.state.on, true);
});

test("a refused request is not remembered as entered", async () => {
  const fake = fakeBackend(false);
  fake.state.refuse = true;
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  await settle();
  controller.toggle();
  await settle();
  assert.equal(seen.at(-1), false);
  fake.state.refuse = false;
  controller.disable();
  await settle();
  assert.deepEqual(fake.log.writes, [true], "nothing was entered, so nothing is left");
});

test("leaving by the button or Esc forgets the entry, so closing later does not touch the window again", async () => {
  const fake = fakeBackend(false);
  const { controller } = controllerWith(fake);
  controller.enable();
  await settle();
  controller.toggle();
  await settle();
  controller.exit();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false]);
  fake.state.on = true; // the owner then enters fullscreen for their own reasons
  controller.disable();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false], "that later fullscreen is the owner's");
  assert.equal(fake.state.on, true);
});

test("toggling twice leaves, and the window the owner leaves with F11 is not left again on close", async () => {
  const fake = fakeBackend(false);
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  await settle();
  controller.toggle();
  await settle();
  controller.toggle();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false]);
  assert.equal(seen.at(-1), false);
  controller.toggle();
  await settle();
  fake.external(false); // F11 from the OS
  await settle();
  assert.equal(seen.at(-1), false);
  controller.disable();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false, true], "already left by the owner: no extra request");
});

test("a view that goes away while the window is still changing does not strand it in fullscreen", async () => {
  const fake = fakeBackend(false);
  let finish: () => void = () => {};
  fake.state.delay = new Promise<void>((resolve) => (finish = resolve));
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  await settle();
  controller.toggle();
  await settle();
  controller.disable(); // unmounted before the request finished
  fake.state.delay = null;
  finish();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false], "the late fullscreen is undone");
  assert.equal(fake.state.on, false);
  assert.ok(!seen.includes(true), "and the late answer never reaches the unmounted view");
});

test("answers and watcher reports that arrive after the view closed are ignored", async () => {
  const fake = fakeBackend(false);
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  controller.disable();
  fake.external(true);
  await settle();
  assert.deepEqual(seen, [], "no state is set on a closed view");
  assert.equal(fake.watching(), false);
  controller.toggle();
  controller.exit();
  await settle();
  assert.deepEqual(fake.log.writes, [], "a closed view asks for nothing");
});

test("opening the view again after closing it starts clean", async () => {
  const fake = fakeBackend(false);
  const { controller, seen } = controllerWith(fake);
  controller.enable();
  controller.enable(); // idempotent while enabled
  assert.equal(fake.log.watchers, 1);
  await settle();
  controller.toggle();
  await settle();
  controller.disable();
  await settle();
  controller.enable();
  await settle();
  assert.equal(fake.log.watchers, 2);
  assert.equal(seen.at(-1), false);
  controller.disable();
  await settle();
  assert.deepEqual(fake.log.writes, [true, false], "the second viewing entered nothing");
});
