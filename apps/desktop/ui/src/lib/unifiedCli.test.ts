import assert from "node:assert/strict";
import test from "node:test";
import type { AdminCliEngine, CliEngineDto } from "../api/types.ts";
import { localCliPlace, mergeCodingClis, placeLabel, serverCliPlace } from "./unifiedCli.ts";

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
test("an unreachable or pending server is never reported as not installed", () => {
  const local = [det("claude", true)];
  for (const serverStatus of ["unreachable", "loading"] as const) {
    // A stale or empty list must not turn into "not installed" rows either.
    const [row] = mergeCodingClis(local, [], { serverStatus });
    assert.equal(row.serverPlace, serverStatus);
    assert.equal(mergeCodingClis(local, [host("codex", true)], { serverStatus }).length, 1);
  }
  assert.equal(mergeCodingClis(local, [], { serverStatus: "ready" })[0].serverPlace, "not_installed");
});

test("rows from the server stay when this computer cannot be asked, and say so", () => {
  const [row] = mergeCodingClis([], [host("codex", true)], { localDown: true });
  assert.equal(row.localPlace, "unreachable");
  assert.equal(row.serverPlace, "logged_in");
});

test("placeLabel reads as one line with a space after each colon", () => {
  const t = (key: string, o?: Record<string, unknown>) =>
    ({
      "settingsUi.codingClis.thisComputer": "This computer:",
      "settingsUi.codingClis.myServer": `${o?.name}:`,
      "settingsUi.codingClis.local.not_installed": "not installed",
      "settingsUi.codingClis.local.off": "off",
      "settingsUi.codingClis.server.logged_in": "signed in",
      "settingsModal.ai.detected.status.notInstalled": "Not installed",
      "settingsModal.ai.detected.status.signIn": "Needs sign-in"
    })[key] ?? key;
  const [missing] = mergeCodingClis([det("a", false)], [host("a", true)]);
  assert.equal(placeLabel(t, missing, "Praxi host", true), "This computer: not installed · Praxi host: signed in");
  assert.equal(placeLabel(t, missing, "Praxi host", false), "This computer: not installed");
  const [signedOut] = mergeCodingClis([det("b", true, "signed_out", false)], null);
  assert.equal(placeLabel(t, signedOut, "", false), "This computer: off (Needs sign-in)");
});

test("placeLabel says when a sign-in was only seen as a file, locally or on the server", () => {
  const t = (key: string, o?: Record<string, unknown>) =>
    ({
      "settingsUi.codingClis.thisComputer": "This computer:",
      "settingsUi.codingClis.myServer": `${o?.name}:`,
      "settingsUi.codingClis.local.on": "on",
      "settingsUi.codingClis.server.unknown": "unknown",
      "serverConnectionStatus.credentialFound": "sign-in file found, unverified"
    })[key] ?? key;
  const seen = det("copilot", true, "signed_in", true);
  (seen.detected as unknown as Record<string, unknown>).auth = "unknown";
  (seen.detected as unknown as Record<string, unknown>).detail = "auth_metadata_only";
  const other = det("claude", true, "signed_in", true);
  (other.detected as unknown as Record<string, unknown>).auth = "unknown";
  (other.detected as unknown as Record<string, unknown>).detail = "auth_metadata_only";
  const [copilot] = mergeCodingClis([seen], null);
  assert.equal(placeLabel(t, copilot, "", false), "This computer: on (sign-in file found, unverified)");
  // Only Copilot and Antigravity read their sign-in from a file; for the others "unknown" says nothing.
  const [claude] = mergeCodingClis([other], null);
  assert.equal(placeLabel(t, claude, "", false), "This computer: on");
  const withFile = { ...host("claude", true, null), credential_file_present: true } as AdminCliEngine;
  const [onServer] = mergeCodingClis([det("claude", false)], [withFile]);
  assert.match(placeLabel(t, onServer, "Praxi host", true), /Praxi host: sign-in file found, unverified$/);
  const noFile = { ...host("claude", true, null), credential_file_present: false } as AdminCliEngine;
  const [plain] = mergeCodingClis([det("claude", false)], [noFile]);
  assert.match(placeLabel(t, plain, "Praxi host", true), /Praxi host: unknown$/);
});
