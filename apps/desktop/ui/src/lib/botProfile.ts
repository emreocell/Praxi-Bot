import type {
  Bot,
  PermissionCategoryName,
  PermissionLevel,
  PermissionPreset
} from "../api/types";
import { PERMISSION_CATEGORIES } from "../api/types";

/** `provider/model` split into its two parts (either may be empty). */
export interface ModelRef {
  provider: string;
  model: string;
}

export type PresetChoice = PermissionPreset | "custom";

export interface BotDraft {
  name: string;
  persona: string;
  color: string;
  /** Image / GIF as a data URL; `null` = initials. */
  avatar: string | null;
  planner: ModelRef;
  executor: ModelRef;
  classifier: ModelRef;
  fallback: string;
  preset: PresetChoice;
  categories: Record<PermissionCategoryName, PermissionLevel>;
  /** Per-bot waiver of the egress gate. Default off. */
  allowEgressAfterReads: boolean;
  computer: "local";
  browser: "local";
}

export const BOT_COLORS = [
  "#3b82f6",
  "#10b981",
  "#8b5cf6",
  "#f59e0b",
  "#e11d48",
  "#0ea5e9"
] as const;

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
    color: str(b.color) || BOT_COLORS[0],
    avatar: str(b.avatar) || null,
    planner: parseModelRef(str(models.planner)),
    executor: parseModelRef(str(models.executor)),
    classifier: parseModelRef(str(models.classifier)),
    fallback: fallback.join(", "),
    preset,
    categories,
    allowEgressAfterReads: perms.allow_egress_after_reads === true,
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
  permissions.allow_egress_after_reads = draft.allowEgressAfterReads;

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
  color: string;
  avatar?: string;
  initials?: string;
  executor: string;
}

/** What the sidebar / chat header need from a stored bot. */
export function botView(bot: Bot): BotView {
  const d = draftFromBot(bot);
  return {
    id: bot.id,
    name: bot.name,
    color: d.color,
    avatar: d.avatar ?? undefined,
    executor: formatModelRef(d.executor)
  };
}
