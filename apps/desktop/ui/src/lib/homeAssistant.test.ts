import assert from "node:assert/strict";
import { test } from "node:test";
import { canSaveHomeAssistant, formatServiceList, isServiceName, looksLikeAddress, parseServiceList } from "./homeAssistant.ts";

const SAVED = { configured: true, url: "http://homeassistant.local:8123", token_set: true };

test("a service is exactly domain.service and the generic homeassistant domain is never listed", () => {
  assert.equal(isServiceName("cover.open_cover"), true);
  assert.equal(isServiceName("lock.unlock"), true);
  for (const bad of ["cover", "cover.", ".open", "a.b.c", "Cover.Open", "cover open", "homeassistant.turn_on", ""]) {
    assert.equal(isServiceName(bad), false, bad);
  }
});

test("the list is split on lines, commas and spaces, lowercased and de-duplicated", () => {
  const parsed = parseServiceList("Cover.Open_Cover\n cover.open_cover , lock.unlock;  \n\nscript.goodnight");
  assert.deepEqual(parsed.services, ["cover.open_cover", "lock.unlock", "script.goodnight"]);
  assert.deepEqual(parsed.invalid, []);
  assert.deepEqual(parseServiceList("").services, []);
});

test("bad names are reported once and never listed", () => {
  const parsed = parseServiceList("cover.open_cover\nlock\nhomeassistant.turn_on\nlock");
  assert.deepEqual(parsed.services, ["cover.open_cover"]);
  assert.deepEqual(parsed.invalid, ["lock", "homeassistant.turn_on"]);
  assert.equal(formatServiceList(parsed.services), "cover.open_cover");
});

test("an address is http or https with a host and no login", () => {
  for (const good of ["http://homeassistant.local:8123", "https://ha.example.com", " http://192.168.1.10:8123/ "]) {
    assert.equal(looksLikeAddress(good), true, good);
  }
  for (const bad of ["", "homeassistant.local:8123", "ftp://ha.local", "http://", "https://user:pw@ha.example.com", "not a url"]) {
    assert.equal(looksLikeAddress(bad), false, bad);
  }
});

test("a token is needed for a new setup and whenever the address changes", () => {
  assert.equal(canSaveHomeAssistant(null, "http://192.168.1.10:8123", "", []), false);
  assert.equal(canSaveHomeAssistant(null, "http://192.168.1.10:8123", "tok", []), true);
  assert.equal(canSaveHomeAssistant({ configured: false, url: null, token_set: false }, "http://192.168.1.10:8123", "", []), false);
  // The same address keeps the stored token, however it is spelled.
  assert.equal(canSaveHomeAssistant(SAVED, "http://homeassistant.local:8123", "", []), true);
  assert.equal(canSaveHomeAssistant(SAVED, "http://HomeAssistant.local:8123/", "", []), true);
  // Another address (host, port or scheme) needs the token again.
  assert.equal(canSaveHomeAssistant(SAVED, "http://homeassistant.local:8124", "", []), false);
  assert.equal(canSaveHomeAssistant(SAVED, "http://other.local:8123", "", []), false);
  assert.equal(canSaveHomeAssistant(SAVED, "https://homeassistant.local:8123", "", []), false);
  assert.equal(canSaveHomeAssistant(SAVED, "http://other.local:8123", "tok", []), true);
  // A stored token that is missing cannot be kept.
  assert.equal(canSaveHomeAssistant({ ...SAVED, token_set: false }, SAVED.url, "", []), false);
});

test("a bad address or a bad service name blocks saving", () => {
  assert.equal(canSaveHomeAssistant(SAVED, "homeassistant.local", "tok", []), false);
  assert.equal(canSaveHomeAssistant(SAVED, SAVED.url, "", ["lock"]), false);
});
