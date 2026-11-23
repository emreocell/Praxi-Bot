import assert from "node:assert/strict";
import { test } from "node:test";
import { DESKTOP_SESSION_COMMAND, explainScreen } from "./screenReason.ts";

test("no display on a server is explained and comes with the exact session command", () => {
  const e = explainScreen("unavailable", "no_display");
  assert.equal(e?.detailKey, "liveView.noScreen.no_display");
  assert.equal(e?.fixKey, "liveView.noScreenFix.addSession");
  assert.equal(e?.command, DESKTOP_SESSION_COMMAND);
  assert.match(DESKTOP_SESSION_COMMAND, /install-linux\.sh --update --with-desktop-session$/);
});

test("a bare 'unavailable' still says what to check instead of repeating itself", () => {
  const e = explainScreen("unavailable", null);
  assert.equal(e?.fixKey, "liveView.noScreenFix.generic");
});

test("a specific reason is left to the viewer; other codes carry no command", () => {
  assert.equal(explainScreen("browser_idle", null), null);
  assert.equal(explainScreen("unavailable", "wayland")?.command, null);
});
