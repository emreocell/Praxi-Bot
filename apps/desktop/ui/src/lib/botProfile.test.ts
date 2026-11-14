import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { BOT_SHAPE_IDS, botShape, fallbackBotShape } from "ui/bot-shape";
import type { Bot, ProvidersDto } from "../api/types.ts";
import {
  AUTONOMY_LEVELS,
  autonomyToPreset,
  BOT_TEMPLATES,
  botView,
  configFromDraft,
  draftFromBot,
  onboardingBotDraft,
  presetToAutonomy,
  resolveAutoExecutorRef
} from "./botProfile.ts";

function policyBot(permissions?: Record<string, unknown>): Bot {
  return {
    id: "policy-bot",
    name: "Restricted",
    created_at_ms: 1,
    config: {
      bot: { id: "restricted", name: "Restricted" },
      models: { executor: "anthropic/old-model" },
      ...(permissions ? { permissions } : {})
    }
  };
}

test("changing only the executor preserves preset category denials and initiator overrides", () => {
  const permissions = {
    preset: "full",
    categories: {
      shell: { level: { level: "deny" } },
      messaging: {
        level: { level: "allow" },
        by_initiator: { schedule: { level: "deny" }, subagent: { level: "ask" } }
      }
    }
  };
  const bot = policyBot(permissions);
  const draft = draftFromBot(bot);
  draft.executor = { provider: "anthropic", model: "new-model" };
  const config = configFromDraft(draft, bot);
  assert.deepEqual(config.permissions, permissions);
  assert.equal((config.models as Record<string, unknown>).executor, "anthropic/new-model");
});

test("editing identity preserves unsupported limits without changing them into ordinary Ask", () => {
  const permissions = {
    preset: "full",
    categories: {
      fs: {
        level: {
          level: "allow_with_limits",
          max_per_hour: 3,
          max_amount_minor: null,
          path_patterns: ["/workspace/reports/**"]
        }
      }
    }
  };
  const bot = policyBot(permissions);
  const draft = draftFromBot(bot);
  assert.equal(draft.categories.fs, "ask");
  draft.name = "Renamed";
  assert.deepEqual(configFromDraft(draft, bot).permissions, permissions);
});

test("editing a bot that inherits the workspace policy keeps that inheritance", () => {
  const bot = policyBot();
  const draft = draftFromBot(bot);
  draft.color = "#4c8dff";
  const config = configFromDraft(draft, bot);
  assert.equal(Object.hasOwn(config, "permissions"), false);
});

test("editing one custom category preserves other rules, restrictions and absent categories", () => {
  const permissions = {
    categories: {
      shell: { level: { level: "deny" } },
      messaging: {
        level: { level: "allow" },
        by_initiator: { schedule: { level: "deny" } }
      }
    }
  };
  const bot = policyBot(permissions);
  const draft = draftFromBot(bot);
  draft.categories.shell = "ask";
  assert.deepEqual(configFromDraft(draft, bot).permissions, {
    categories: {
      ...permissions.categories,
      shell: { level: { level: "ask" } }
    }
  });
  // Updating a base rule must not erase its per-initiator restriction either.
  draft.categories.messaging = "ask";
  const config = configFromDraft(draft, bot);
  assert.deepEqual((config.permissions as typeof permissions).categories.messaging, {
    level: { level: "ask" },
    by_initiator: { schedule: { level: "deny" } }
  });
});

test("a preset with category overrides is shown as custom with the effective base levels", () => {
  const bot = policyBot({ preset: "full", categories: { shell: { level: { level: "deny" } } } });
  const draft = draftFromBot(bot);
  assert.equal(draft.preset, "custom");
  assert.equal(draft.categories.shell, "deny");
  assert.equal(draft.categories.messaging, "allow");
  draft.categories.fs = "deny";
  assert.deepEqual(configFromDraft(draft, bot).permissions, {
    preset: "full",
    categories: { shell: { level: { level: "deny" } }, fs: { level: { level: "deny" } } }
  });
});

test("choosing custom starts from the selected preset's levels", () => {
  const bot = policyBot({ preset: "read-only" });
  const draft = draftFromBot(bot);
  assert.ok(Object.values(draft.categories).every((level) => level === "deny"));
  draft.preset = "custom";
  draft.categories.fs = "ask";
  const config = configFromDraft(draft, bot);
  assert.equal((config.permissions as { preset: string }).preset, "read-only");
  assert.deepEqual((config.permissions as { categories: unknown }).categories, {
    fs: { level: { level: "ask" } }
  });
});

test("an explicit preset replacement and a new custom bot still save their requested policy", () => {
  const bot = policyBot({ preset: "full", categories: { shell: { level: { level: "deny" } } } });
  const draft = draftFromBot(bot);
  draft.preset = "read-only";
  assert.deepEqual(configFromDraft(draft, bot).permissions, { preset: "read-only" });
  const fresh = draftFromBot();
  fresh.name = "New";
  fresh.preset = "custom";
  fresh.categories.shell = "deny";
  assert.deepEqual((configFromDraft(fresh).permissions as { categories: Record<string, unknown> }).categories.shell, {
    level: { level: "deny" }
  });
});

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

function loadLocale(name: "en" | "tr") {
  return JSON.parse(
    readFileSync(new URL(`../i18n/locales/${name}.json`, import.meta.url), "utf8")
  ) as {
    botForm: { preset: Record<string, { name: string; hint: string }> };
    onboarding: { step3: { explain: Record<string, string> } };
    newBot: {
      autonomyLevels: Record<string, string>;
      autonomyHints: Record<string, string>;
    };
  };
}

test("autonomy hints in both locales accurately describe the stored permission presets", () => {
  for (const level of AUTONOMY_LEVELS) {
    assert.equal(presetToAutonomy(autonomyToPreset(level)), level);
  }

  const en = loadLocale("en");
  const tr = loadLocale("tr");

  // Finding 1 regression checks:
  // - `ask` (`read-and-draft`) and `balanced` (`approve-destructive`) resolve to
  //   the same all-Ask policy (`Effect::Read` skips ask; drafts, writes, deletes,
  //   page fetches, clicks, commands, and sends all ask).
  assert.doesNotMatch(en.newBot.autonomyHints.ask, /prepares drafts, and asks/i);
  assert.doesNotMatch(en.newBot.autonomyHints.ask, /before changing files, browsing, or sending/i);
  assert.match(en.newBot.autonomyHints.ask, /draft/i);
  assert.match(en.newBot.autonomyHints.ask, /Balanced/i);

  assert.doesNotMatch(tr.newBot.autonomyHints.ask, /taslak hazırlar;/i);
  assert.doesNotMatch(tr.newBot.autonomyHints.ask, /taramadan ve göndermeden önce sorar/i);
  assert.match(tr.newBot.autonomyHints.ask, /taslak/i);
  assert.match(tr.newBot.autonomyHints.ask, /Dengeli/i);

  assert.doesNotMatch(
    en.newBot.autonomyHints.balanced,
    /asks before sending messages or deleting\.?$/i
  );
  assert.match(en.newBot.autonomyHints.balanced, /draft/i);
  assert.match(en.newBot.autonomyHints.balanced, /command/i);
  assert.match(en.newBot.autonomyHints.balanced, /\bAsk\b/);

  assert.doesNotMatch(
    tr.newBot.autonomyHints.balanced,
    /göndermeden ve silmeden önce sorar\.?$/i
  );
  assert.match(tr.newBot.autonomyHints.balanced, /taslak/i);
  assert.match(tr.newBot.autonomyHints.balanced, /komut/i);
  assert.match(tr.newBot.autonomyHints.balanced, /Sorar/);

  // - `auto` (`full`) runs ordinary file deletes, shell commands, and message
  //   sends without asking, pausing only for Money, schedule activations, and
  //   catastrophic never-list commands.
  assert.doesNotMatch(
    en.newBot.autonomyHints.auto,
    /pausing only for money or high-risk commands/i
  );
  assert.match(en.newBot.autonomyHints.auto, /delet/i);
  assert.match(en.newBot.autonomyHints.auto, /send/i);
  assert.match(en.newBot.autonomyHints.auto, /Money/);
  assert.match(en.newBot.autonomyHints.auto, /never-list/i);

  assert.doesNotMatch(
    tr.newBot.autonomyHints.auto,
    /yalnızca para ve yüksek riskli komutlarda durur/i
  );
  assert.match(tr.newBot.autonomyHints.auto, /silme/i);
  assert.match(tr.newBot.autonomyHints.auto, /gönder/i);
  assert.match(tr.newBot.autonomyHints.auto, /Para/);
  assert.match(tr.newBot.autonomyHints.auto, /yasaklı/i);

  // `botForm.preset.read-and-draft` and onboarding explain must not claim drafts run free or changes are denied.
  assert.doesNotMatch(en.botForm.preset["read-and-draft"].hint, /never sends or changes/i);
  assert.doesNotMatch(tr.botForm.preset["read-and-draft"].hint, /asla göndermez/i);
  assert.doesNotMatch(en.onboarding.step3.explain["read-and-draft"], /read and prepare drafts/i);
  assert.doesNotMatch(tr.onboarding.step3.explain["read-and-draft"], /taslak hazırlayabilir/i);
});

test("botView preserves stored shape when it differs from fallbackBotShape(id)", () => {
  assert.equal(fallbackBotShape("bot-cube-0"), "cube");
  const bot: Bot = {
    id: "bot-cube-0",
    name: "HeartBot",
    created_at_ms: 1,
    config: {
      bot: { id: "heartbot", name: "HeartBot", shape: "heart", color: "#4c8dff" },
      models: { executor: "anthropic/claude-sonnet-5-5" }
    }
  };
  const view = botView(bot);
  assert.equal(view.shape, "heart");
  assert.equal(botShape(view.id, view.shape).id, "heart");
});

test("onboarding Step 4 preview shape matches configFromDraft stored shape", () => {
  // Without an explicit shape, `new-bot` hashes to `ghost`, whereas `configFromDraft` stores `pebble`.
  assert.equal(fallbackBotShape("new-bot"), "ghost");

  const draft = onboardingBotDraft(undefined, {
    name: "Atlas",
    color: "#f4f4f5",
    persona: "Helpful",
    executor: { provider: "anthropic", model: "claude-sonnet-5-5" },
    planner: { provider: "", model: "" },
    classifier: { provider: "", model: "" },
    preset: "approve-destructive"
  });
  const stored = configFromDraft(draft, undefined);
  assert.equal(draft.shape, "pebble");
  assert.equal((stored.bot as Record<string, unknown>).shape, draft.shape);

  const existing: Bot = {
    id: "bot-cube-0",
    name: "Atlas",
    created_at_ms: 1,
    config: {
      bot: { id: "atlas", name: "Atlas", shape: "heart", color: "#4c8dff" },
      models: { executor: "anthropic/claude-sonnet-5-5" }
    }
  };
  const editedDraft = onboardingBotDraft(existing, {
    name: "Atlas",
    color: "#4c8dff",
    persona: "Helpful",
    executor: { provider: "anthropic", model: "claude-sonnet-5-5" },
    planner: { provider: "", model: "" },
    classifier: { provider: "", model: "" },
    preset: "approve-destructive"
  });
  assert.equal(editedDraft.shape, "heart");

  const onboardingSrc = readFileSync(
    new URL("../views/OnboardingView.tsx", import.meta.url),
    "utf8"
  );
  const previewMatch = onboardingSrc.match(/<BotAvatar[\s\S]*?size="lg"/);
  assert.ok(previewMatch, "Onboarding Step 4 should render BotAvatar preview");
  assert.match(
    previewMatch[0],
    /\bshape\s*:/,
    "Onboarding Step 4 BotAvatar preview must pass explicit shape matching configFromDraft"
  );
});
