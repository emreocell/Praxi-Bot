import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { BOT_SHAPE_IDS, botShape, fallbackBotShape } from "ui/bot-shape";
import type { Bot, ProvidersDto } from "../api/types.ts";
import {
  AUTONOMY_LEVELS,
  autonomyToPreset,
  BOT_TEMPLATES,
  botHasFullAccess,
  botView,
  configFromDraft,
  draftFromBot,
  presetToAutonomy,
  resolveAutoExecutorRef
} from "./botProfile.ts";

function policyBot(permissions?: Record<string, unknown>): Bot {
  return {
    id: "policy-bot",
    name: "Restricted",
    created_at_ms: 1,
    version: 1,
    config: {
      bot: { id: "restricted", name: "Restricted" },
      models: { executor: "anthropic/old-model" },
      ...(permissions ? { permissions } : {})
    }
  };
}

test("schedule actions use Full access only when explicit or inherited", () => {
  assert.equal(botHasFullAccess(policyBot({ preset: "full" }), null), true);
  assert.equal(botHasFullAccess(policyBot(), "full"), true);
  assert.equal(botHasFullAccess(policyBot({ preset: "read-and-draft" }), "full"), false);
  assert.equal(botHasFullAccess(policyBot({ categories: { fs: { level: "deny" } } }), "full"), false);
  assert.equal(botHasFullAccess(policyBot(), null), false);
  assert.equal(botHasFullAccess(undefined, "full"), false);
});

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
    version: 1,
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
    accessLevel: Record<string, { name: string; line: string; can: string[]; stops: string[] }>;
  };
}

test("access level texts in both locales accurately describe the stored permission presets", () => {
  for (const level of AUTONOMY_LEVELS) {
    assert.equal(presetToAutonomy(autonomyToPreset(level)), level);
  }

  const en = loadLocale("en").accessLevel;
  const tr = loadLocale("tr").accessLevel;
  const all = (l: { line: string; can: string[]; stops: string[] }) => [l.line, ...l.can, ...l.stops].join(" ");

  // Careful (`read-and-draft`) asks before every change and send; it never claims drafts are
  // sent or changes are denied.
  assert.match(all(en["read-and-draft"]), /change/i);
  assert.match(all(en["read-and-draft"]), /send/i);
  assert.doesNotMatch(all(en["read-and-draft"]), /never sends or changes/i);
  assert.match(all(tr["read-and-draft"]), /değişiklik/i);
  assert.match(all(tr["read-and-draft"]), /gönder/i);

  // Balanced (`approve-destructive`) runs workspace edits, browsing and its own computer, and
  // asks for deletes, sends, money and the user's PC.
  assert.match(all(en["approve-destructive"]), /work area/i);
  assert.match(all(en["approve-destructive"]), /own computer/i);
  assert.match(all(en["approve-destructive"]), /delet/i);
  assert.match(all(tr["approve-destructive"]), /çalışma alanı/i);
  assert.match(all(tr["approve-destructive"]), /kendi bilgisayarında/i);
  assert.match(all(tr["approve-destructive"]), /silme/i);

  // Full (`full`) deletes and sends without asking; money still asks, the never-list and the
  // owner's blocks still hold, and authority-minting asks after untrusted content.
  assert.match(all(en.full), /delet/i);
  assert.match(all(en.full), /send/i);
  assert.match(all(en.full), /money/i);
  assert.match(all(en.full), /untrusted/i);
  assert.match(all(tr.full), /siler/i);
  assert.match(all(tr.full), /gönderir/i);
  assert.match(all(tr.full), /para/i);
  assert.match(all(tr.full), /güvenilmeyen/i);
});

test("botView preserves stored shape when it differs from fallbackBotShape(id)", () => {
  assert.equal(fallbackBotShape("bot-cube-0"), "cube");
  const bot: Bot = {
    id: "bot-cube-0",
    name: "HeartBot",
    created_at_ms: 1,
    version: 1,
    config: {
      bot: { id: "heartbot", name: "HeartBot", shape: "heart", color: "#4c8dff" },
      models: { executor: "anthropic/claude-sonnet-5-5" }
    }
  };
  const view = botView(bot);
  assert.equal(view.shape, "heart");
  assert.equal(botShape(view.id, view.shape).id, "heart");
});

test("the Can create bots switch round-trips through tools.manage_bots and keeps tool filters", () => {
  const bot = policyBot();
  (bot.config as Record<string, unknown>).tools = { allow: ["fs_read"], exclude: ["shell_run"] };
  (bot.config as Record<string, Record<string, unknown>>).bot.created_by = "maker-id";
  const d = draftFromBot(bot);
  assert.equal(d.canManageBots, false);
  d.canManageBots = true;
  const on = configFromDraft(d, bot);
  assert.deepEqual(on.tools, { allow: ["fs_read"], exclude: ["shell_run"], manage_bots: true });
  // The editor never rewrites who made the bot.
  assert.equal((on.bot as Record<string, unknown>).created_by, "maker-id");
  const again = draftFromBot({ ...bot, config: on });
  assert.equal(again.canManageBots, true);
  again.canManageBots = false;
  assert.deepEqual(configFromDraft(again, { ...bot, config: on }).tools, {
    allow: ["fs_read"],
    exclude: ["shell_run"]
  });
});

test("a bot that never had tools gets no tools section until the switch is on", () => {
  const bot = policyBot();
  const d = draftFromBot(bot);
  assert.equal("tools" in configFromDraft(d, bot), false);
  d.canManageBots = true;
  assert.deepEqual(configFromDraft(d, bot).tools, { manage_bots: true });
});
