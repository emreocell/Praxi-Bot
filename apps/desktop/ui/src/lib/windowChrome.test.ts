import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { defaultWindowFrame, maximizeLabelKey, trackMaximized, type ChromeWindow } from "./windowChrome.ts";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function fakeWindow() {
  let maximized = false;
  let listener: (() => void) | undefined;
  let unlistened = 0;
  const win: ChromeWindow = {
    isMaximized: async () => maximized,
    onResized: async (handler) => {
      listener = handler;
      return () => { unlistened += 1; };
    }
  };
  return {
    win,
    set: (value: boolean) => { maximized = value; listener?.(); },
    unlistened: () => unlistened
  };
}

test("the maximize button switches between maximize and restore labels", () => {
  assert.equal(maximizeLabelKey(false), "window.maximize");
  assert.equal(maximizeLabelKey(true), "window.restore");
});

test("the maximized state follows resizes and reports changes only", async () => {
  const fake = fakeWindow();
  const seen: boolean[] = [];
  const stop = trackMaximized(fake.win, (value) => seen.push(value));
  await flush();
  fake.set(true);
  fake.set(true);
  await flush();
  fake.set(false);
  await flush();
  assert.deepEqual(seen, [false, true, false]);
  stop();
  assert.equal(fake.unlistened(), 1);
});

test("stopping before the listener is ready still removes it", async () => {
  const fake = fakeWindow();
  const stop = trackMaximized(fake.win, () => {});
  stop();
  await flush();
  assert.equal(fake.unlistened(), 1);
});

test("only Windows defaults to the app's own window controls", () => {
  assert.equal(defaultWindowFrame("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"), false);
  assert.equal(defaultWindowFrame("Mozilla/5.0 (X11; Linux x86_64)"), true);
  assert.equal(defaultWindowFrame("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"), true);
  assert.equal(defaultWindowFrame(""), true);
});

test("the Windows window config turns the native frame off and the base config keeps it", () => {
  const read = (name: string) => JSON.parse(readFileSync(new URL(`../../../${name}`, import.meta.url), "utf8")) as
    { app: { windows: { label: string; decorations?: boolean; resizable?: boolean }[] } };
  const win = read("tauri.windows.conf.json").app.windows[0];
  assert.equal(win?.label, "main");
  assert.equal(win?.decorations, false);
  assert.equal(win?.resizable, true);
  assert.notEqual(read("tauri.conf.json").app.windows[0]?.decorations, false);
});

test("every window control has a label in all three locales", () => {
  for (const locale of ["en", "tr", "de"]) {
    const window = (JSON.parse(readFileSync(new URL(`../i18n/locales/${locale}.json`, import.meta.url), "utf8")) as
      { window: Record<string, string> }).window;
    for (const key of ["minimize", "maximize", "restore", "close"]) assert.ok(window[key], `${locale}.window.${key}`);
  }
});
