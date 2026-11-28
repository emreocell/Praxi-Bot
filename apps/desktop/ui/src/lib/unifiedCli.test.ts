import assert from "node:assert/strict";
import test from "node:test";
import type { AdminCliEngine, CliEngineDto } from "../api/types.ts";
import { localCliPlace, mergeCodingClis, serverCliPlace } from "./unifiedCli.ts";

const det = (
  id: string,
  installed: boolean,
  auth: "signed_in" | "signed_out" = "signed_in",
  enabled = true
): CliEngineDto =>
  ({
    detected: { id, display_name: id, installed, auth, runnable: true, account_label: null, version: null },
    enabled,
    outdated: false,
    login: "",
    docs_url: "",
    user_disabled: !enabled,
    executable_names: [],
    version_args: [],
    auth_args: []
  }) as CliEngineDto;

const host = (id: string, installed: boolean, signed_in: boolean | null = true): AdminCliEngine =>
  ({
    id,
    name: id,
    installed,
    signed_in,
    can_install: true,
    login: "",
    login_flow: "manual",
    docs_url: ""
  }) as AdminCliEngine;

test("localCliPlace", () => {
  assert.equal(localCliPlace(null).place, "not_installed");
  assert.equal(localCliPlace(det("a", false)).place, "not_installed");
  assert.equal(localCliPlace(det("b", true, "signed_in", true)).place, "on");
  assert.equal(localCliPlace(det("c", true, "signed_in", false)).place, "off");
  assert.equal(localCliPlace(det("d", true, "signed_out", false)).detail, "sign_in");
});

test("serverCliPlace", () => {
  assert.equal(serverCliPlace(null), "not_installed");
  assert.equal(serverCliPlace(host("a", false)), "not_installed");
  assert.equal(serverCliPlace(host("b", true, true)), "logged_in");
  assert.equal(serverCliPlace(host("c", true, false)), "not_logged_in");
});

test("mergeCodingClis unions ids and prefers installed sort", () => {
  const merged = mergeCodingClis([det("claude", true)], [host("codex", true), host("claude", true)]);
  assert.equal(merged.length, 2);
  assert.equal(merged[0].id, "claude");
  assert.ok(merged[0].local);
  assert.ok(merged[0].server);
  assert.equal(merged[1].id, "codex");
  assert.equal(merged[1].local, null);
});