import assert from "node:assert/strict";
import { test } from "node:test";
import { QUALITY_MODES } from "./liveViewAdaptive.ts";
import { loadQualityMode, saveQualityMode } from "./liveViewQualityPref.ts";

function mapStorage(store = new Map<string, string>()): Pick<Storage, "getItem" | "setItem"> {
  return {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => {
      store.set(key, value);
    }
  };
}

test("loadQualityMode and saveQualityMode round-trip every valid mode", () => {
  const storage = mapStorage();
  assert.equal(loadQualityMode(storage), "auto");

  for (const mode of QUALITY_MODES) {
    saveQualityMode(mode, storage);
    assert.equal(loadQualityMode(storage), mode);
  }
});

test("loadQualityMode falls back to auto on unknown stored values", () => {
  const store = new Map<string, string>([["praxi.liveView.quality", "ultra"]]);
  assert.equal(loadQualityMode(mapStorage(store)), "auto");
});

test("loadQualityMode and saveQualityMode tolerate throwing or null storage", () => {
  const throwingStorage: Pick<Storage, "getItem" | "setItem"> = {
    getItem: () => {
      throw new Error("Storage blocked");
    },
    setItem: () => {
      throw new Error("Quota exceeded");
    }
  };

  assert.equal(loadQualityMode(throwingStorage), "auto");
  assert.doesNotThrow(() => saveQualityMode("saver", throwingStorage));

  assert.equal(loadQualityMode(null), "auto");
  assert.doesNotThrow(() => saveQualityMode("high", null));
});
