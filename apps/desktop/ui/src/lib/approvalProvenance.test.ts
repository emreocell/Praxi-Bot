import assert from "node:assert/strict";
import { test } from "node:test";
import type { ApprovalGrant, ToolItem } from "../state/timeline.ts";
import { decisionLine } from "./approvalProvenance.ts";

const category = (key: string) => key;
const tool = (grant: ApprovalGrant | undefined, outcome: "approved" | "denied" = "approved") => ({
  name: "shell_run",
  input: {},
  approvalDecision: { outcome, offsetMs: 3000, ...(grant ? { grant } : {}) }
}) as Pick<ToolItem, "name" | "input" | "approvalDecision">;

test("the step line says who or what granted the call, one line per grant kind", () => {
  const line = (grant: ApprovalGrant | undefined, time: string | null = "3 s") =>
    decisionLine(tool(grant), time, category);
  assert.deepEqual(line({ by: "owner_card" }), { key: "chat.chain.grant.ownerCard", values: { time: "3 s" } });
  assert.deepEqual(line({ by: "owner_card" }, null), { key: "chat.chain.grant.ownerCardNoTime", values: {} });
  assert.deepEqual(line({ by: "full_access", level: "full" }), { key: "chat.chain.grant.fullAccess", values: {} });
  assert.equal(line({ by: "owner_schedule" })?.key, "chat.chain.grant.ownerSchedule");
  assert.equal(line({ by: "owner_mission" })?.key, "chat.chain.grant.ownerMission");
  assert.equal(line({ by: "balanced_level" })?.key, "chat.chain.grant.balancedLevel");
  assert.equal(line({ by: "standing_grant" })?.key, "chat.chain.grant.standingGrant");
  assert.equal(line({ by: "site_rule" })?.key, "chat.chain.grant.siteRule");
  // An automatic grant does not depend on a recorded time.
  assert.equal(line({ by: "full_access" }, null)?.key, "chat.chain.grant.fullAccess");
});

test("a remembered choice names its rule: the tool, the category, this chat or this task", () => {
  const line = (rule: ApprovalGrant["rule"]) =>
    decisionLine(tool({ by: "remembered_choice", ...(rule ? { rule } : {}) }), "1 s", category);
  assert.deepEqual(line({ scope: "always", name: "shell_run" }), {
    key: "chat.chain.grant.rememberedAlways", values: { what: "shell run" }
  });
  assert.deepEqual(line({ scope: "chat" }), { key: "chat.chain.grant.rememberedChat", values: { what: "shell run" } });
  assert.deepEqual(line({ scope: "task" }), { key: "chat.chain.grant.rememberedTask", values: { what: "shell run" } });
  assert.deepEqual(line({ scope: "category", name: "fs" }), {
    key: "chat.chain.grant.rememberedCategory", values: { what: "botForm.category.fs" }
  });
  // A category this build has no label for, or no rule at all: the plain wording, never the raw id.
  assert.equal(line({ scope: "category", name: "PRIVATE" })?.key, "chat.chain.grant.remembered");
  assert.deepEqual(line(undefined), { key: "chat.chain.grant.remembered", values: {} });
});

test("an approval of unknown provenance stays a plain approval, and a denial never claims a grant", () => {
  assert.deepEqual(decisionLine(tool(undefined), "3 s", category), {
    key: "chat.chain.approvedAt", values: { time: "3 s" }
  });
  assert.equal(decisionLine(tool(undefined), null, category), null);
  assert.deepEqual(decisionLine(tool({ by: "full_access" }, "denied"), "3 s", category), {
    key: "chat.chain.deniedAt", values: { time: "3 s" }
  });
  assert.equal(decisionLine({ name: "shell_run", input: {} }, "3 s", category), null);
});
