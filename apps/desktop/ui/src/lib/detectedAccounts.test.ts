import assert from "node:assert/strict";
import { test } from "node:test";
import { MockApiClient } from "../api/mock.ts";
import type { DetectedProviderDto, ProvidersDto } from "../api/types.ts";
import {
  configuredCliIds,
  detectedRowAction,
  signInCommand
} from "./detectedAccounts.ts";

function row(
  overrides: Partial<DetectedProviderDto> = {}
): DetectedProviderDto {
  return {
    id: "claude",
    display_name: "Claude Code",
    installed: true,
    version: "2.1.0",
    auth: "signed_in",
    account_label: "work@example.com",
    runnable: true,
    detail: null,
    message: null,
    ...overrides
  };
}

test("detectedRowAction uses runnable from the daemon and configured list", () => {
  const empty = new Set<string>();
  const configured = new Set<string>(["cli:claude"]);

  assert.deepEqual(detectedRowAction(row(), empty), { kind: "enable" });
  assert.deepEqual(
    detectedRowAction(row({ auth: "unknown" }), empty),
    { kind: "enable" }
  );
  assert.deepEqual(detectedRowAction(row(), configured), { kind: "enabled" });
  assert.deepEqual(
    detectedRowAction(row({ auth: "signed_out" }), empty),
    { kind: "sign_in_hint" }
  );
  assert.deepEqual(
    detectedRowAction(row({ installed: false, auth: "unknown", version: null }), empty),
    { kind: "not_installed" }
  );
  assert.deepEqual(
    detectedRowAction(
      row({
        id: "codex",
        display_name: "Codex",
        runnable: false,
        detail: "codex_unconfinable",
        message: "The installed Codex cannot disable unified_exec, so Praxi will not run it."
      }),
      empty
    ),
    { kind: "codex_unconfinable" }
  );
  assert.deepEqual(
    detectedRowAction(
      row({ id: "cursor", display_name: "Cursor", runnable: false, detail: null }),
      empty
    ),
    { kind: "not_runnable" }
  );
});

test("configuredCliIds collects enabled cli:* ids and ignores disabled ones", () => {
  const dto = {
    active: ["cli:claude"],
    catalog: [],
    default_provider: null,
    configured: [
      {
        id: "cli:claude",
        enabled: true,
        is_default: false,
        default_model: null,
        has_key: false,
        custom: false,
        active: true,
        models: [],
        models_error: null
      },
      {
        id: "cli:codex",
        enabled: false,
        is_default: false,
        default_model: null,
        has_key: false,
        custom: false,
        active: false,
        models: [],
        models_error: null
      }
    ]
  } as ProvidersDto;
  const set = configuredCliIds(dto);
  assert.equal(set.has("cli:claude"), true);
  assert.equal(set.has("cli:codex"), false);
  assert.equal(signInCommand("claude"), "claude");
  assert.equal(signInCommand("codex"), "codex login");
});

test("MockApiClient serves detected providers and enables claude idempotently while refusing codex", async () => {
  const client = new MockApiClient();
  const rows = await client.detectedProviders(true);
  assert.equal(rows.length, 6);
  const claude = rows.find((r) => r.id === "claude");
  assert.equal(claude?.runnable, true);

  const enabled = await client.enableCliProvider("claude");
  assert.equal(enabled.id, "cli:claude");
  assert.equal(enabled.has_key, false);

  const providers = await client.providers();
  assert.ok(providers.configured.some((p) => p.id === "cli:claude" && !p.has_key));
  assert.ok(providers.catalog.some((c) => c.id === "cli:claude" && c.kind === "cli"));

  await assert.rejects(() => client.enableCliProvider("codex"));
});
