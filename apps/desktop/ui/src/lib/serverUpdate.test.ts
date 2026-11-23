import assert from "node:assert/strict";
import { test } from "node:test";
import type { HostSetupJob, HostSetupStep } from "../api/types";
import { rowStates, serverIsOlder, updateCommands, updateFailure, updateState } from "./serverUpdate.ts";

const step = (id: string, status: HostSetupStep["status"], detail = ""): HostSetupStep => ({ id, title: id, status, detail, hint: null });
const job = (state: HostSetupJob["state"], steps: HostSetupStep[]) => ({ state, steps });

test("only a strictly older, understood server version shows the update badge", () => {
  assert.equal(serverIsOlder("0.0.9", "0.1.0"), true);
  assert.equal(serverIsOlder("0.1.0-alpha.1", "0.1.0"), true);
  assert.equal(serverIsOlder("0.1.0", "0.1.0"), false);
  assert.equal(serverIsOlder("0.2.0", "0.1.0"), false, "never offer a downgrade");
  assert.equal(serverIsOlder("weird", "0.1.0"), false);
  assert.equal(serverIsOlder(null, "0.1.0"), false);
});

test("progress rows follow what the installer reported", () => {
  assert.deepEqual(Object.values(rowStates(job("running", []))), ["running", "pending", "pending", "pending"]);
  assert.deepEqual(Object.values(rowStates(job("running", [step("hostkey", "pass"), step("connect", "pass")]))), ["done", "running", "pending", "pending"]);
  assert.deepEqual(
    Object.values(rowStates(job("running", [step("connect", "pass"), step("install.snapshot", "pass"), step("install.run", "pass")]))),
    ["done", "done", "done", "running"]
  );
  assert.deepEqual(
    Object.values(rowStates(job("failed", [step("connect", "pass"), step("install.run", "fail", "boom")]))),
    ["done", "done", "failed", "pending"]
  );
  assert.deepEqual(Object.values(rowStates(job("succeeded", [step("health.version", "pass")]))), ["done", "done", "done", "done"]);
});

test("a failed update gets a plain reason; a verification failure is never called a network problem", () => {
  assert.equal(updateFailure(job("running", [])), null);
  assert.equal(updateFailure(job("failed", [step("connect", "fail", "refused")])), "unreachable");
  assert.equal(updateFailure(job("failed", [step("hostkey", "fail", "not accepted")])), "host_key");
  assert.equal(updateFailure(job("failed", [step("install.run", "fail", "attestation verification failed | rollback complete")])), "verify");
  assert.equal(updateFailure(job("failed", [step("install.run", "fail", "exit 1: rollback complete; previously active services restarted")])), "rolled_back");
  assert.equal(updateFailure(job("failed", [step("install.run", "fail", "exit 1")])), "generic");
});

test("copy-paste commands pin the verified bootstrap to the app's own release and refuse odd tags", () => {
  const text = updateCommands("v0.1.0-alpha.1") ?? "";
  assert.match(text, /version=v0\.1\.0-alpha\.1/);
  assert.match(text, /gh attestation verify/);
  assert.match(text, /--update --version "\$version"/);
  assert.equal(updateCommands("v1.0.0; rm -rf /"), null);
  assert.equal(updateCommands(null), null);
});

test("the release tag decides when every build reports the same cargo version", () => {
  const app = { version: "0.1.0", releaseTag: "v0.1.0-alpha.4" };
  assert.equal(updateState({ version: "0.1.0", releaseTag: "v0.1.0-alpha.3" }, app), "older");
  assert.equal(updateState({ version: "0.1.0", releaseTag: "v0.1.0-alpha.4" }, app), "current");
  assert.equal(updateState({ version: "0.1.0", releaseTag: "v0.1.0-alpha.5" }, app), "newer");
  assert.equal(updateState({ version: "0.1.0", releaseTag: null }, app), "unknown", "never claim up to date without a tag");
  assert.equal(updateState({ version: "0.1.0" }, { version: "0.1.0", releaseTag: null }), "current", "local build: versions only");
});
