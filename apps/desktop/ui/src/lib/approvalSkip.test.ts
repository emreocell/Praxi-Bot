import assert from "node:assert/strict";
import { test } from "node:test";
import type { TimelineItem } from "../state/timeline.ts";
import {
  isRedirectDenial,
  REDIRECT_DENY_REASON,
  runWasRedirected
} from "./approvalSkip.ts";

const tool = (denyReason: string | undefined): TimelineItem =>
  ({
    kind: "tool",
    key: "t",
    callId: "c1",
    name: "shell_run",
    input: {},
    status: "denied",
    requestedMs: 1,
    gated: true,
    denyReason
  }) as TimelineItem;

test("redirect denial reason matches the daemon contract exactly", () => {
  assert.equal(REDIRECT_DENY_REASON, "denied: the owner wrote a new message instead");
  assert.equal(isRedirectDenial(REDIRECT_DENY_REASON), true);
  assert.equal(isRedirectDenial("denied: stopped by you"), false);
  assert.equal(isRedirectDenial(null), false);
  assert.equal(isRedirectDenial(undefined), false);
});

test("runWasRedirected is true only when a tool card used the redirect reason", () => {
  assert.equal(runWasRedirected([tool(REDIRECT_DENY_REASON)]), true);
  assert.equal(runWasRedirected([tool("denied: stopped by you")]), false);
  assert.equal(
    runWasRedirected([
      { kind: "text", key: "a", ts: 0, text: "hi" } as TimelineItem,
      tool(REDIRECT_DENY_REASON)
    ]),
    true
  );
  assert.equal(runWasRedirected([]), false);
});