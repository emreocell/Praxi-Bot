import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { sessionRequest } from "./liveViewRequest.ts";

test("the open viewer asks for the host's full 1920x1080 cap at quality 80", () => {
  const watch = sessionRequest({ viewerOpen: true, owning: false });
  assert.deepEqual(watch, { maxWidth: 1920, maxHeight: 1080, quality: 80 });
  const drive = sessionRequest({ viewerOpen: true, owning: true });
  assert.deepEqual(drive, watch);
});

test("the card asks for a smaller picture than the viewer", () => {
  const card = sessionRequest({ viewerOpen: false, owning: false });
  const viewer = sessionRequest({ viewerOpen: true, owning: false });
  assert.ok(card.maxWidth < viewer.maxWidth && card.maxHeight < viewer.maxHeight);
});

test("the stream picture is drawn without CSS filters or pixelation", () => {
  const css = readFileSync(new URL("../views/LiveViewer.module.css", import.meta.url), "utf8");
  const rule = css.slice(css.indexOf(".stageImg"), css.indexOf("}", css.indexOf(".stageImg")));
  assert.doesNotMatch(rule, /filter|blur|pixelated|transform/);
  assert.match(rule, /image-rendering: auto/);
});

test("the expanded viewer asks for the large profile and the card for the small one", () => {
  const card = sessionRequest({ viewerOpen: false, owning: false });
  const viewer = sessionRequest({ viewerOpen: true, owning: false });
  assert.ok(viewer.maxWidth >= 1920 && viewer.maxHeight >= 1080);
  assert.ok(viewer.maxWidth > card.maxWidth);
  assert.ok(viewer.quality >= 80 && viewer.quality > card.quality);
});
