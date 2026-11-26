import assert from "node:assert/strict";
import { test } from "node:test";
import {
  calendarPresetUrl,
  cleanServer,
  isValidCalendarName,
  suggestCalendarName
} from "./calendarPresets.ts";

test("presets build the service's own address", () => {
  const me = { username: "me@example.com", server: "" };
  assert.equal(calendarPresetUrl("icloud", me), "https://caldav.icloud.com/");
  assert.equal(
    calendarPresetUrl("fastmail", me),
    "https://caldav.fastmail.com/dav/calendars/user/me%40example.com/"
  );
  assert.equal(
    calendarPresetUrl("google", me),
    "https://apidata.googleusercontent.com/caldav/v2/me%40example.com/events/"
  );
  assert.equal(
    calendarPresetUrl("nextcloud", { username: "ann", server: "https://cloud.example.com/index.php" }),
    "https://cloud.example.com/remote.php/dav/calendars/ann/"
  );
});

test("an address is left empty until every part it needs is known", () => {
  assert.equal(calendarPresetUrl("fastmail", { username: " ", server: "" }), "");
  assert.equal(calendarPresetUrl("nextcloud", { username: "ann", server: "" }), "");
  assert.equal(calendarPresetUrl("other", { username: "ann", server: "x" }), "");
  // iCloud needs no user name in the address.
  assert.equal(calendarPresetUrl("icloud", { username: "", server: "" }), "https://caldav.icloud.com/");
});

test("a server typed as an address is reduced to its host", () => {
  assert.equal(cleanServer("  HTTPS://Cloud.Example.com:8443/nextcloud/?a=1 "), "Cloud.Example.com:8443");
  assert.equal(cleanServer("cloud.example.com"), "cloud.example.com");
});

test("names follow the daemon's rules and do not clash", () => {
  assert.equal(suggestCalendarName("icloud", []), "icloud");
  assert.equal(suggestCalendarName("icloud", ["icloud", "icloud-2"]), "icloud-3");
  assert.equal(suggestCalendarName("other", []), "calendar");
  assert.ok(isValidCalendarName("work_2-a"));
  for (const bad of ["", "a b", "a/b", "x".repeat(41)]) {
    assert.equal(isValidCalendarName(bad), false, bad);
  }
});
