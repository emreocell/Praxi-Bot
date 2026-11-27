import assert from "node:assert/strict";
import { test } from "node:test";
import { homeStatus } from "./home.ts";
import {
  computerScopePlan,
  deriveHostState,
  hostAcceptsWork,
  liveRunIds,
  type HostHealthLike,
  type HostLinkLike,
  olderSide
} from "./hostState.ts";

const link = (state: HostLinkLike["state"]): HostLinkLike => ({
  id: "h1",
  name: "Oracle",
  state
});
const health = (over: Partial<HostHealthLike> = {}): HostHealthLike => ({
  status: "ok",
  version: "0.1.0",
  api_version: "1",
  ...over
});
const base = { localVersion: "0.1.0", localApiVersion: "1" };

test("a host that is not in the paired list, or revoked us, is not paired", () => {
  assert.equal(
    deriveHostState({ link: null, health: null, ...base }).kind,
    "not_paired"
  );
  assert.equal(
    deriveHostState({ link: link("unreachable"), health: null, ...base }).kind,
    "not_paired"
  );
});

test("offline, and connecting until the host answered health", () => {
  assert.equal(
    deriveHostState({ link: link("offline"), health: null, ...base }).kind,
    "offline"
  );
  for (const state of ["idle", "connecting"] as const) {
    assert.equal(
      deriveHostState({ link: link(state), health: null, ...base }).kind,
      "connecting"
    );
  }
  // Channel up but the health probe has not come back yet.
  assert.equal(
    deriveHostState({ link: link("connected"), health: null, ...base }).kind,
    "connecting"
  );
});

test("connected and compatible", () => {
  const s = deriveHostState({ link: link("connected"), health: health(), ...base });
  assert.equal(s.kind, "connected");
  assert.equal(hostAcceptsWork(s), true);
});

test("a different API or minor version blocks and shows both versions", () => {
  for (const h of [health({ api_version: "2" }), health({ version: "0.2.0" })]) {
    const s = deriveHostState({ link: link("connected"), health: h, ...base });
    assert.equal(s.kind, "version_mismatch");
    if (s.kind === "version_mismatch") {
      assert.equal(s.local, "0.1.0");
      assert.equal(s.host, h.version);
      assert.equal(s.blocking, true);
    }
    assert.equal(hostAcceptsWork(s), false);
  }
});

test("a patch-only difference warns but does not block", () => {
  const s = deriveHostState({
    link: link("connected"),
    health: health({ version: "0.1.3" }),
    ...base
  });
  assert.equal(s.kind, "version_mismatch");
  assert.equal(hostAcceptsWork(s), true);
});

test("published preview builds use the exact release, including legacy build IDs", () => {
  const derive = (remote: Partial<HostHealthLike>) => deriveHostState({
    link: link("connected"), health: health(remote), ...base, localRelease: "v0.1.0-alpha.10"
  });
  assert.deepEqual(derive({ build_id: "v0.1.0-alpha.5" }), {
    kind: "version_mismatch", local: "v0.1.0-alpha.10", host: "v0.1.0-alpha.5", blocking: true, update: "host"
  });
  assert.equal(derive({ release_tag: "v0.1.0-alpha.10" }).kind, "connected");
  assert.equal(hostAcceptsWork(derive({ release_tag: "v0.1.0-alpha.11" })), false);
  assert.equal(olderSide("v0.1.0-alpha.10", "v0.1.0-alpha.11"), "app");
  assert.equal(hostAcceptsWork(derive({ release_tag: "v0.1.0-alpha.10", api_version: "2" })), false);
  const unidentified = derive({});
  assert.equal(hostAcceptsWork(unidentified), false);
  assert.equal(unidentified.kind === "version_mismatch" && unidentified.unidentified, true);
  assert.equal(homeStatus({ kind: "host", hostId: "h1", name: "Oracle" }, false, unidentified), "version_mismatch");
});

test("a stable app does not take a server that names no release for the same version", () => {
  const derive = (remote: Partial<HostHealthLike>) => deriveHostState({
    link: link("connected"), health: health(remote), ...base, localRelease: "v0.1.0"
  });
  // An unidentified legacy pre-release answers as cargo version 0.1.0, like the stable release.
  for (const remote of [{}, { build_id: "0.1.0+gabc1234" }, { release_tag: null, build_id: null }]) {
    const s = derive(remote);
    assert.equal(s.kind, "version_mismatch");
    assert.equal(hostAcceptsWork(s), false);
    assert.equal(s.kind === "version_mismatch" && s.blocking, true);
    assert.equal(s.kind === "version_mismatch" && s.unidentified, true);
    assert.equal(s.kind === "version_mismatch" && s.update, "host");
  }
  // The same release, however it is reported, still connects; a different patch only warns.
  assert.equal(derive({ release_tag: "v0.1.0" }).kind, "connected");
  assert.equal(derive({ build_id: "v0.1.0" }).kind, "connected");
  const patch = derive({ release_tag: "v0.1.1", version: "0.1.1" });
  assert.equal(patch.kind, "version_mismatch");
  assert.equal(hostAcceptsWork(patch), true);
  // An app that does not know its release (a local build) still compares versions only.
  assert.equal(deriveHostState({ link: link("connected"), health: health(), ...base }).kind, "connected");
});

test("setup_required on a compatible host is the no-model-key state", () => {
  const s = deriveHostState({
    link: link("connected"),
    health: health({ status: "setup_required" }),
    ...base
  });
  assert.equal(s.kind, "no_model_key");
  assert.equal(hostAcceptsWork(s), false);
});

test("a version mismatch wins over a missing key", () => {
  const s = deriveHostState({
    link: link("connected"),
    health: health({ status: "setup_required", api_version: "9" }),
    ...base
  });
  assert.equal(s.kind, "version_mismatch");
});

test("only live runs are listed for stop", () => {
  assert.deepEqual(
    liveRunIds([
      { id: "a", state: "running" },
      { id: "b", state: "completed" },
      { id: "c", state: "waiting_approval" },
      { id: "d", state: "paused" },
      { id: "e", state: "queued" }
    ]),
    ["a", "c", "e"]
  );
});

test("the local control bridge stays mounted while a host is selected", () => {
  assert.deepEqual(computerScopePlan("local"), { localBridge: true, hostSession: false });
  assert.deepEqual(computerScopePlan("host-key"), { localBridge: true, hostSession: true });
});

test("a host that reports starting or degraded startup is shown honestly", () => {
  const derive = (startup: HostHealthLike["startup"]) =>
    deriveHostState({ link: link("connected"), health: health({ startup }), ...base });
  assert.equal(derive("starting").kind, "starting");
  assert.equal(derive("degraded").kind, "degraded");
  assert.equal(derive("ready").kind, "connected");
  assert.equal(hostAcceptsWork(derive("starting")), true);
  const home = { kind: "host", hostId: "h1", name: "Oracle" } as const;
  assert.equal(homeStatus(home, false, derive("degraded")), "degraded");
});

test("the banner can name which side to update", () => {
  assert.equal(olderSide("0.9.2", "0.10.0"), "app");
  assert.equal(olderSide("1.2.0", "1.1.9"), "host");
  assert.equal(olderSide("1.2.0", "1.2.0"), "unknown");
  assert.equal(olderSide("dev", "1.2.0"), "unknown");
});
