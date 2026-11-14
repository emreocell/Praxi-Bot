import {
  CHARACTER_COLORS,
  fallbackBotColor,
  fallbackBotShape,
  normalizeBotShape,
  type BotShapeId
} from "ui/bot-shape";
import type {
  Bot,
  PermissionCategoryName,
  PermissionLevel,
  PermissionPreset,
  ProvidersDto
} from "../api/types.ts";
import { PERMISSION_CATEGORIES } from "../api/types.ts";
import { defaultModelRef, isCliProvider, providerRoleDefaults } from "./providerView.ts";

/** `provider/model` split into its two parts (either may be empty). */
export interface ModelRef {
  provider: string;
  model: string;
}

export type PresetChoice = PermissionPreset | "custom";
export type BotTemplateId = "blank" | "researcher" | "coder" | "assistant" | "social";
export type AutonomyLevel = "ask" | "balanced" | "auto";

export interface BotTemplate {
  id: BotTemplateId;
  defaultName: string;
  shape: BotShapeId;
  color: string;
}

export interface BotDraft {
  name: string;
  persona: string;
  shape: BotShapeId;
  color: string;
  /** Image / GIF as a data URL; `null` = 3D clay character. */
  avatar: string | null;
  planner: ModelRef;
  executor: ModelRef;
  classifier: ModelRef;
  fallback: string;
  preset: PresetChoice;
  categories: Record<PermissionCategoryName, PermissionLevel>;
  computer: "local";
  browser: "local";
}

export const BOT_COLORS = CHARACTER_COLORS;
export const DEFAULT_NEW_BOT_COLOR = "#34c77b";

export const BOT_TEMPLATES: readonly BotTemplate[] = [
  { id: "blank", defaultName: "", shape: "pebble", color: "#f4f4f5" },
  { id: "researcher", defaultName: "Nova", shape: "diamond", color: "#2fc4b2" },
  { id: "coder", defaultName: "Byte", shape: "gem", color: "#4c8dff" },
  { id: "assistant", defaultName: "Mira", shape: "cloud", color: "#f25fa8" },
  { id: "social", defaultName: "Echo", shape: "flower", color: "#f5803e" }
] as const;

export const AUTONOMY_LEVELS: readonly AutonomyLevel[] = ["ask", "balanced", "auto"] as const;

/**
 * Map simple 3-way autonomy to an existing daemon permission preset.
 *
 * Note: `read-and-draft` and `approve-destructive` both set `Ask` on every
 * category in `permission-engine` (`read_tools_skip_ask` lets `Effect::Read`
 * tools run without prompting, while all writes/fetches/drafts/sends/commands
 * ask). `full` sets `Allow` on every category except `Money`, never-list
 * commands, and `always_ask` schedule actions.
 */
export function autonomyToPreset(level: AutonomyLevel): PermissionPreset {
  switch (level) {
    case "ask":
      return "read-and-draft";
    case "balanced":
      return "approve-destructive";
    case "auto":
      return "full";
  }
}

/** Map a stored preset to the 3-way autonomy pill (`null` for `read-only`/`custom`). */
export function presetToAutonomy(preset: PresetChoice): AutonomyLevel | null {
  switch (preset) {
    case "read-and-draft":
      return "ask";
    case "approve-destructive":
      return "balanced";
    case "full":
      return "auto";
    case "read-only":
    case "custom":
      return null;
  }
}

export const PRESET_CHOICES: readonly PresetChoice[] = [
  "read-only",
  "approve-destructive",
  "full",
  "read-and-draft",
  "custom"
] as const;

export function parseModelRef(s: string | undefined | null): ModelRef {
  if (!s) {
    return { provider: "", model: "" };
  }
  const i = s.indexOf("/");
  return i < 0
    ? { provider: "", model: s }
    : { provider: s.slice(0, i), model: s.slice(i + 1) };
}

export function formatModelRef(m: ModelRef): string {
  const model = m.model.trim();
  if (!model) {
    return "";
  }
  return m.provider.trim() ? `${m.provider.trim()}/${model}` : model;
}

type Json = Record<string, unknown>;

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

const DEFAULT_CATEGORIES = Object.fromEntries(
  PERMISSION_CATEGORIES.map((c) => [c, "ask"])
) as Record<PermissionCategoryName, PermissionLevel>;

/** Form state from a stored bot (or defaults for a new one). */
export function draftFromBot(bot?: Bot): BotDraft {
  const cfg = obj(bot?.config);
  const b = obj(cfg.bot);
  const models = obj(cfg.models);
  const perms = obj(cfg.permissions);
  const cats = obj(perms.categories);
  const presetName = str(perms.preset);
  const categories = { ...DEFAULT_CATEGORIES };
  for (const c of PERMISSION_CATEGORIES) {
    const level = str(obj(obj(cats[c]).level).level);
    if (level === "allow" || level === "ask" || level === "deny") {
      categories[c] = level;
    }
  }
  const hasCats = Object.keys(cats).length > 0;
  let preset: PresetChoice = "approve-destructive";
  if (presetName) {
    preset = presetName.replace(/_/g, "-") as PermissionPreset;
  } else if (hasCats) {
    preset = "custom";
  }
  const fallback = Array.isArray(models.fallback)
    ? models.fallback.filter((x): x is string => typeof x === "string")
    : [];
  return {
    name: bot?.name ?? "",
    persona: str(b.persona),
    shape: normalizeBotShape(str(b.shape)) ?? (bot ? fallbackBotShape(bot.id) : "pebble"),
    color: str(b.color) || (bot ? fallbackBotColor(bot.id) : DEFAULT_NEW_BOT_COLOR),
    avatar: str(b.avatar) || null,
    planner: parseModelRef(str(models.planner)),
    executor: parseModelRef(str(models.executor)),
    classifier: parseModelRef(str(models.classifier)),
    fallback: fallback.join(", "),
    preset,
    categories,
    computer: "local",
    browser: "local"
  };
}

function slug(name: string): string {
  const s = name
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return s || "bot";
}

/**
 * Profile JSON (ADR-0011) from the form.  `existing` keeps every key the form
 * does not know about (tool filters, language, ...), because `PUT` replaces the
 * whole profile.
 */
export function configFromDraft(draft: BotDraft, existing?: Bot): Json {
  const old = obj(existing?.config);
  const oldBot = obj(old.bot);
  const id = str(oldBot.id) || `${slug(draft.name)}-${Math.random().toString(36).slice(2, 7)}`;

  const botSection: Json = {
    ...oldBot,
    id,
    name: draft.name.trim(),
    persona: draft.persona,
    shape: normalizeBotShape(draft.shape) ?? "pebble",
    color: draft.color
  };
  if (draft.avatar) {
    botSection.avatar = draft.avatar;
  } else {
    delete botSection.avatar;
  }

  const models: Json = { ...obj(old.models) };
  models.executor = formatModelRef(draft.executor);
  for (const [key, ref] of [
    ["planner", draft.planner],
    ["classifier", draft.classifier]
  ] as const) {
    const v = formatModelRef(ref);
    if (v) {
      models[key] = v;
    } else {
      delete models[key];
    }
  }
  const fallback = draft.fallback
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (fallback.length > 0) {
    models.fallback = fallback;
  } else {
    delete models.fallback;
  }

  const permissions: Json = {};
  if (draft.preset === "custom") {
    permissions.categories = Object.fromEntries(
      PERMISSION_CATEGORIES.map((c) => [
        c,
        { level: { level: draft.categories[c] } }
      ])
    );
  } else {
    permissions.preset = draft.preset;
  }

  return {
    ...old,
    bot: botSection,
    models,
    permissions,
    runtime: {
      ...obj(old.runtime),
      computer: draft.computer,
      browser: draft.browser
    }
  };
}

export interface BotView {
  id: string;
  name: string;
  shape: BotShapeId;
  color: string;
  avatar?: string;
  initials?: string;
  executor: string;
}

/** Pick the daemon's default `provider/model` for "Auto" brain mode. */
export function resolveAutoExecutorRef(
  dto: ProvidersDto | null,
  fakeAvailable = false
): ModelRef {
  const def = defaultModelRef(dto);
  if (def) {
    return parseModelRef(def);
  }
  const firstEnabled = dto?.configured.find((p) => p.enabled);
  if (firstEnabled) {
    const cli = isCliProvider(firstEnabled.id, dto);
    const model =
      firstEnabled.default_model ||
      firstEnabled.models[0] ||
      (cli ? "default" : providerRoleDefaults(firstEnabled.id).defaultModel);
    if (model) {
      return { provider: firstEnabled.id, model };
    }
  }
  if (fakeAvailable) {
    return { provider: "fake", model: "echo" };
  }
  return { provider: "", model: "" };
}

/** What the sidebar / chat header need from a stored bot. */
export function botView(bot: Bot): BotView {
  const d = draftFromBot(bot);
  return {
    id: bot.id,
    name: bot.name,
    shape: d.shape,
    color: d.color,
    avatar: d.avatar ?? undefined,
    executor: formatModelRef(d.executor)
  };
}

/** Fields edited in Onboarding Step 4; shared by the preview and save path. */
export interface OnboardingBotFields {
  name: string;
  color: string;
  avatar?: string;
  persona: string;
  executor: ModelRef;
  planner: ModelRef;
  classifier: ModelRef;
  preset: PermissionPreset;
}

/**
 * Build the `BotDraft` used by Onboarding Step 4 so the preview `<BotAvatar>`
 * and `configFromDraft` agree on `shape` and `color`.
 */
export function onboardingBotDraft(
  existing: Bot | undefined,
  fields: OnboardingBotFields
): BotDraft {
  const base = draftFromBot(existing);
  return {
    ...base,
    name: fields.name.trim(),
    color: fields.color,
    avatar: fields.avatar ?? null,
    persona: fields.persona,
    executor: fields.executor,
    planner: fields.planner,
    classifier: fields.classifier,
    preset: fields.preset
  };
}
