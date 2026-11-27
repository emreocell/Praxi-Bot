import assert from "node:assert/strict";
import { test } from "node:test";
import { checksDone, runPairChecks, type PairCheckRow, type ServerProbe } from "./serverChecks.ts";

const probe = (over: Partial<ServerProbe> = {}): ServerProbe => ({
  connect: async () => undefined,
  status: async () => ({ build: { version: "0.1.0", api_version: "1", status: "ok" }, hasDisplay: true }),
  localVersion: async () => ({ localVersion: "0.1.0", localApiVersion: "1" }),
  canSendModels: async () => true,
  ...over
});
const states = (rows: PairCheckRow[]) => rows.map((r) => r.state);

test("a healthy server passes all four checks, in order", async () => {
  const seen: string[] = [];
  const rows = await runPairChecks(probe(), (r) => seen.push(states(r).join(",")));
  assert.deepEqual(states(rows), ["pass", "pass", "pass", "pass"]);
  assert.equal(seen[0], "running,pending,pending,pending");
  assert.ok(checksDone(rows));
});

test("an unreachable server stops at the first row", async () => {
  const rows = await runPairChecks(
    probe({
      connect: async () => {
        throw new Error("offline");
      }
    }),
    () => undefined
  );
  assert.deepEqual(states(rows), ["fail", "pending", "pending", "pending"]);
  assert.equal(checksDone(rows), false);
});

test("a status that cannot be read counts as unreachable", async () => {
  const rows = await runPairChecks(
    probe({
      status: async () => {
        throw new Error("boom");
      }
    }),
    () => undefined
  );
  assert.equal(rows[0].state, "fail");
});

test("version, screen and model problems are advice, not failures", async () => {
  const rows = await runPairChecks(
    probe({
      status: async () => ({ build: { version: "0.0.9", api_version: "1", status: "ok" }, hasDisplay: false }),
      canSendModels: async () => {
        throw new Error("denied");
      }
    }),
    () => undefined
  );
  assert.deepEqual(states(rows), ["pass", "warn", "warn", "warn"]);
  assert.ok(checksDone(rows));
});

test("pairing indicates an older prerelease even when the package versions match", async () => {
  const rows = await runPairChecks(probe({
    status: async () => ({ build: { version: "0.1.0", api_version: "1", build_id: "v0.1.0-alpha.4", status: "ok" }, hasDisplay: true }),
    localVersion: async () => ({ localVersion: "0.1.0", localApiVersion: "1", localRelease: "v0.1.0-alpha.5" })
  }), () => {});
  assert.deepEqual(states(rows), ["pass", "warn", "pass", "pass"]);
});
