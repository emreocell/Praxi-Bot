import assert from "node:assert/strict";
import { test } from "node:test";
import { BOT_SHAPE_IDS, fallbackBotShape } from "ui/bot-shape";
import type { Bot, ProvidersDto } from "../api/types.ts";
import {
  autonomyToPreset,
  BOT_TEMPLATES,
  botView,
  configFromDraft,
  draftFromBot,
  presetToAutonomy,
  resolveAutoExecutorRef
} from "./botProfile.ts";

test("draftFromBot and configFromDraft round-trip shape and color with legacy fallback", () => {
  const legacy: Bot = {
    id: "legacy-bot-123",
    name: "Legacy",
    created_at_ms: 1,
    config: {
      bot: { id: "legacy", name: "Legacy", persona: "Helpful" },
      models: { executor: "anthropic/claude-sonnet-5-5" }
    }
  };
  const draft = draftFromBot(legacy);
  assert.equal(draft.shape, fallbackBotShape("legacy-bot-123"));
  assert.match(draft.color, /^#[0-9a-f]{6}$/i);

  const edited = configFromDraft({ ...draft, shape: "ears", color: "#4c8dff" }, legacy);
  assert.equal((edited.bot as Record<string, unknown>).shape, "ears");
  assert.equal((edited.bot as Record<string, unknown>).color, "#4c8dff");

  const stored: Bot = { ...legacy, config: edited };
  assert.equal(botView(stored).shape, "ears");
  assert.equal(botView(stored).color, "#4c8dff");
});

test("BOT_TEMPLATES use valid 16-set character shapes and valid hex colors", () => {
  assert.equal(BOT_TEMPLATES.length, 5);
  for (const tpl of BOT_TEMPLATES) {
    assert.ok(BOT_SHAPE_IDS.includes(tpl.shape));
    assert.match(tpl.color, /^#[0-9a-f]{6}$/i);
  }
});

test("autonomy maps cleanly onto existing permission presets without overwriting read-only or custom", () => {
  assert.equal(autonomyToPreset("ask"), "read-and-draft");
  assert.equal(autonomyToPreset("balanced"), "approve-destructive");
  assert.equal(autonomyToPreset("auto"), "full");

  assert.equal(presetToAutonomy("read-and-draft"), "ask");
  assert.equal(presetToAutonomy("approve-destructive"), "balanced");
  assert.equal(presetToAutonomy("full"), "auto");
  assert.equal(presetToAutonomy("read-only"), null);
  assert.equal(presetToAutonomy("custom"), null);
});

test("resolveAutoExecutorRef selects default provider model, enabled fallback, or fake", () => {
  const dto = {
    default_provider: "anthropic",
    active: ["anthropic"],
    catalog: [],
    configured: [
      {
        id: "anthropic",
        is_default: true,
        default_model: "claude-sonnet-5-5",
        has_key: true,
        enabled: true,
        models: ["claude-sonnet-5-5"],
        models_error: null
      }
    ]
  } as unknown as ProvidersDto;
  assert.deepEqual(resolveAutoExecutorRef(dto), {
    provider: "anthropic",
    model: "claude-sonnet-5-5"
  });

  const noDefault = {
    default_provider: null,
    active: ["ollama"],
    catalog: [],
    configured: [
      {
        id: "ollama",
        is_default: false,
        default_model: "",
        has_key: false,
        enabled: true,
        models: ["qwen3:8b"],
        models_error: null
      }
    ]
  } as unknown as ProvidersDto;
  assert.deepEqual(resolveAutoExecutorRef(noDefault), {
    provider: "ollama",
    model: "qwen3:8b"
  });

  assert.deepEqual(resolveAutoExecutorRef(null, true), {
    provider: "fake",
    model: "echo"
  });
  assert.deepEqual(resolveAutoExecutorRef(null, false), {
    provider: "",
    model: ""
  });
});
