import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_SCREEN_FIT, loadScreenFit, saveScreenFit, toggleScreenFit } from "./liveViewFitPref.ts";

function mapStorage(store = new Map<string, string>()): Pick<Storage, "getItem" | "setItem"> {
  return {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => {
      store.set(key, value);
    }
  };
}

test("the whole screen is shown (contain) unless the owner chose to crop", () => {
  assert.equal(DEFAULT_SCREEN_FIT, "contain");
  const storage = mapStorage();
  assert.equal(loadScreenFit(storage), "contain");
  saveScreenFit("cover", storage);
  assert.equal(loadScreenFit(storage), "cover");
  saveScreenFit("contain", storage);
  assert.equal(loadScreenFit(storage), "contain");
  assert.equal(loadScreenFit(mapStorage(new Map([["praxi.liveView.screenFit", "stretch"]]))), "contain");
  assert.equal(toggleScreenFit("contain"), "cover");
  assert.equal(toggleScreenFit("cover"), "contain");
});

test("a blocked storage never breaks the choice", () => {
  const throwing: Pick<Storage, "getItem" | "setItem"> = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    }
  };
  assert.equal(loadScreenFit(throwing), "contain");
  assert.doesNotThrow(() => saveScreenFit("cover", throwing));
  assert.equal(loadScreenFit(null), "contain");
});
