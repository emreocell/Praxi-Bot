import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { AUDIT_KINDS, AUDIT_KNOWN_ACTIONS, auditActorKey, auditEventKey, auditQuery } from "./auditLog.ts";

const locale = (l: string) =>
  JSON.parse(readFileSync(new URL(`../i18n/locales/${l}.json`, import.meta.url), "utf8")) as {
    audit: { event: Record<string, string>; kind: Record<string, string> };
  };

test("every known action and kind has a plain name in en, tr and de", () => {
  for (const l of ["en", "tr", "de"]) {
    const a = locale(l).audit;
    for (const action of AUDIT_KNOWN_ACTIONS) {
      const key = auditEventKey(action).replace("audit.event.", "");
      assert.ok(a.event[key]?.length > 0, `${l}: ${action}`);
    }
    assert.ok(a.event.unknown);
    for (const k of AUDIT_KINDS) assert.ok(a.kind[k], `${l}: kind ${k}`);
  }
});

test("unknown actions fall back to the generic name; devices keep their own name", () => {
  assert.equal(auditEventKey("grant.always"), "audit.event.grant_always");
  assert.equal(auditEventKey("something.new"), "audit.event.unknown");
  assert.equal(auditActorKey("user"), "audit.actor.user");
  assert.equal(auditActorKey("peer:abc"), null);
});

test("the query leaves empty filters out and adds paging", () => {
  const empty = { windowMs: null, actor: "", kind: "", bot: "", q: "  " };
  assert.deepEqual(auditQuery(empty, 1000), {});
  assert.deepEqual(
    auditQuery({ ...empty, windowMs: 400, kind: "import", q: " x " }, 1000, { offset: 50, limit: 50 }),
    { since_ms: 600, kind: "import", q: "x", offset: 50, limit: 50 }
  );
});
