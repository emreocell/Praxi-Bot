import assert from "node:assert/strict";
import { test } from "node:test";
import type { HostSetupJob, HostSetupStep } from "../api/types";
import {
  checkForm,
  checklist,
  diagnose,
  initialWizard,
  logLines,
  progressPercent,
  wizardReducer,
  type WizardState
} from "./serverWizard.ts";

const step = (id: string, status: HostSetupStep["status"], detail = ""): HostSetupStep => ({
  id,
  title: id,
  status,
  detail,
  hint: null
});
const job = (state: HostSetupJob["state"], steps: HostSetupStep[]): HostSetupJob => ({
  id: "j",
  state,
  target: "ubuntu@h:22",
  steps,
  notes: [],
  host_key: null,
  host_id: null,
  host_name: null,
  view_targets: null,
  install_available: false
});
const run = (s: WizardState, ...a: Parameters<typeof wizardReducer>[1][]) => a.reduce(wizardReducer, s);

test("the wizard explains first, offers three paths and can always go back", () => {
  let s = run(initialWizard, { type: "next" });
  assert.equal(s.page, "choose");
  s = run(s, { type: "choose", path: "oracle" });
  assert.equal(s.page, "oracle");
  s = run(s, { type: "next" });
  assert.equal(s.page, "details");
  assert.equal(run(s, { type: "back" }).page, "oracle");
  assert.equal(run(initialWizard, { type: "next" }, { type: "choose", path: "ssh" }).page, "details");
  assert.equal(run(initialWizard, { type: "next" }, { type: "choose", path: "home" }).page, "home");
  assert.equal(run(s, { type: "run" }, { type: "back" }).page, "details");
});

test("live validation catches the usual typing mistakes with a specific reason", () => {
  const base = { host: "203.0.113.7", user: "ubuntu", port: "22", auth: "key" as const, keyPath: "~/.ssh/id" };
  assert.equal(checkForm(base).ok, true);
  assert.equal(checkForm({ ...base, host: "ssh://1.2.3.4" }).host, "hostScheme");
  assert.equal(checkForm({ ...base, host: "ubuntu@1.2.3.4" }).host, "hostUser");
  assert.equal(checkForm({ ...base, host: "1.2.3.4:2222" }).host, "hostPort");
  assert.equal(checkForm({ ...base, port: "99999" }).port, "portInvalid");
  assert.equal(checkForm({ ...base, keyPath: "~/.ssh/id.pub" }).key, "keyPublic");
  assert.equal(checkForm({ ...base, auth: "agent", keyPath: "" }).ok, true);
});

test("the pre-flight checklist follows the engine and stops at the first problem", () => {
  const mid = checklist(job("running", [step("hostkey", "pass"), step("connect", "pass"), step("preflight.os", "pass")]));
  assert.deepEqual(
    mid.map((c) => c.state),
    ["pass", "pass", "pass", "running", "pending"]
  );
  const bad = checklist(job("failed", [step("hostkey", "pass"), step("connect", "fail")]));
  assert.deepEqual(
    bad.map((c) => c.state),
    ["pass", "fail", "pending", "pending", "pending"]
  );
});

test("login and network failures get an exact, plain diagnosis", () => {
  const typed = { auth: "key" as const, port: "22", user: "ubuntu" };
  const login = job("failed", [step("connect", "fail", "the server rejected the login (key or user)")]);
  assert.equal(diagnose(login, typed)?.code, "auth_key");
  assert.equal(diagnose(login, { ...typed, auth: "agent" })?.code, "auth_agent");
  const perms = job("failed", [step("connect", "fail", "WARNING: UNPROTECTED PRIVATE KEY FILE! bad permissions")]);
  assert.equal(diagnose(perms, typed)?.code, "key_permissions");
  const slow = job("failed", [step("hostkey", "fail", "could not reach the server: connection timed out")]);
  assert.equal(diagnose(slow, typed)?.code, "port_closed");
  const refused = job("failed", [step("hostkey", "fail", "could not reach the server: connection refused")]);
  assert.equal(diagnose(refused, { ...typed, port: "2222" })?.code, "wrong_port");
  assert.equal(diagnose(job("cancelled", []), typed), null);
});

test("progress only goes up with finished rows and never claims 100 before success", () => {
  const a = progressPercent(job("running", [step("hostkey", "pass")]));
  const b = progressPercent(job("running", [step("hostkey", "pass"), step("connect", "pass"), step("preflight.os", "pass")]));
  assert.ok(b > a && b < 100);
  assert.equal(progressPercent(job("succeeded", [])), 100);
  assert.deepEqual(logLines(job("running", [step("connect", "pass", "ok")])), ["[pass] connect: ok"]);
});
