import type { BotShapeId } from "ui/bot-shape";
import type { Bot, BotPatchInput, PermissionPreset } from "../api/types.ts";
import { ACCESS_LEVELS } from "./accessLevels.ts";
import type { PresetChoice } from "./botProfile.ts";
import { type ImportInput, type ImportOutput, initialLevel, type SharedProfile, type SharePreset } from "./profileShare.ts";

/**
 * The New-bot dialog as one short page: name and look, what it should do (a general helper or a
 * ready-made bot), how freely it may work, Create.  Everything else waits under "More options".
 * Pure, so the rules are tested without a browser.
 */

/** The choice that is not a gallery template: a general helper the owner instructs. */
export const GENERAL_CHOICE = "general";

/** Gallery templates with a short purpose line of their own (the long description stays in the review). */
export const PURPOSE_TEMPLATES = ["researcher", "inbox-triage", "daily-briefing", "coder", "family-helper"] as const;

/** i18n key of a choice's one-line purpose, or `null` for a template this app does not know yet. */
export function purposeKey(choice: string): string | null {
  if (choice === GENERAL_CHOICE) return "newBot.quick.purpose.general";
  return (PURPOSE_TEMPLATES as readonly string[]).includes(choice) ? `newBot.quick.purpose.${choice}` : null;
}

/**
 * The access level a choice starts at: the general helper at the recommended level, a gallery
 * template at what it asks for but never above "approve-destructive" (`initialLevel`).
 */
export function startLevel(choice: string, requested: string | null | undefined, recommended: PermissionPreset): PermissionPreset {
  if (choice === GENERAL_CHOICE) return recommended;
  return initialLevel("template", isSharePreset(requested) ? requested : undefined);
}

const SHARE_PRESETS: readonly string[] = ["read-only", "read-and-draft", "approve-destructive", "full"];

function isSharePreset(v: unknown): v is SharePreset {
  return typeof v === "string" && SHARE_PRESETS.includes(v);
}

/**
 * The ceiling sent with a template import.  The page offers only the four levels; anything else
 * (a stale "custom") falls back to where the template starts, never to "full".
 */
export function importLevel(preset: PresetChoice, requested: string | null | undefined): SharePreset {
  return isSharePreset(preset) ? preset : initialLevel("template", isSharePreset(requested) ? requested : undefined);
}

/** Least to most freedom, to compare two levels. */
const STRENGTH: Record<PermissionPreset, number> = {
  "read-only": 0,
  "read-and-draft": 1,
  "approve-destructive": 2,
  full: 3
};

/** The most a template can get: what it asks for (the daemon's default when it asks for nothing). */
function templateTop(requested: string | null | undefined): PermissionPreset {
  return isSharePreset(requested) ? requested : "approve-destructive";
}

/**
 * The levels a choice can really get, most freedom first.  All four for the general helper; for a
 * template none above what it asks for, because the import grants the lower of the two.
 */
export function levelsFor(choice: string, requested: string | null | undefined): PermissionPreset[] {
  if (choice === GENERAL_CHOICE) return [...ACCESS_LEVELS];
  const top = STRENGTH[templateTop(requested)];
  return ACCESS_LEVELS.filter((l) => STRENGTH[l] <= top);
}

/**
 * The level after picking another choice: the new choice's starting level, unless the owner already
 * chose one; that choice is kept, lowered to what the new choice can get (never raised).
 */
export function levelAfterPick(
  current: PresetChoice,
  ownerChose: boolean,
  choice: string,
  requested: string | null | undefined,
  recommended: PermissionPreset
): PermissionPreset {
  const start = startLevel(choice, requested, recommended);
  if (!ownerChose || !isSharePreset(current)) return start;
  const levels = levelsFor(choice, requested);
  return levels.includes(current) ? current : (levels[0] ?? start);
}

/** A ready-made bot cannot run on a command-line engine: the import refuses one. */
export function templateNeedsOtherModel(choice: string, model: string): boolean {
  return choice !== GENERAL_CHOICE && model.trim().startsWith("cli:");
}

/**
 * The name after picking another choice: a name the owner typed stays; an empty one, or the one the
 * previous pick filled in, becomes the new pick's name.
 */
export function nameAfterPick(current: string, filledByPick: string, next: string): string {
  const typed = current.trim();
  return typed === "" || typed === filledByPick.trim() ? next : current;
}

export interface Look {
  shape: BotShapeId;
  color: string;
  avatar: string | null;
}

/** The look as field-level changes, applied to a bot made from a template (imports carry no look). */
export function lookOps(look: Look): NonNullable<BotPatchInput["ops"]> {
  const ops: NonNullable<BotPatchInput["ops"]> = [
    { op: "set", path: "/bot/shape", value: look.shape },
    { op: "set", path: "/bot/color", value: look.color }
  ];
  if (look.avatar) ops.push({ op: "set", path: "/bot/avatar", value: look.avatar });
  return ops;
}

/** "Surprise me": another shape and another colour, never the current pair. `rand` is in [0, 1). */
export function shuffleLook<C extends string>(
  current: { shape: BotShapeId; color: string },
  shapes: readonly BotShapeId[],
  colors: readonly C[],
  rand: () => number = Math.random
): { shape: BotShapeId; color: C } {
  const pick = <T>(list: readonly T[], not: (x: T) => boolean): T => {
    const pool = list.filter((x) => !not(x));
    const from = pool.length > 0 ? pool : list;
    return from[Math.min(from.length - 1, Math.floor(rand() * from.length))] as T;
  };
  const color = current.color.toLowerCase();
  return {
    shape: pick(shapes, (s) => s === current.shape),
    color: pick(colors, (c) => c.toLowerCase() === color)
  };
}

/** The two daemon calls a template needs (a structural slice of the API client, so tests can fake it). */
export interface TemplateClient {
  importProfile(input: ImportInput): Promise<ImportOutput>;
  patchBot(id: string, input: BotPatchInput): Promise<Bot>;
}

/**
 * Create a bot from a gallery template the way the review view does: an import the daemon clamps to
 * the chosen level, with the owner's model and name and no file access yet; then the chosen look.
 * The look is cosmetic, so a refused look change keeps the created bot (with the default look).
 * The model must not be a command-line engine (see `templateNeedsOtherModel`).
 */
export async function createFromTemplate(
  client: TemplateClient,
  input: { template: SharedProfile; preset: PresetChoice; model: string; name: string; look: Look }
): Promise<Bot> {
  const out = await client.importProfile({
    contents: JSON.stringify(input.template),
    permission_level: importLevel(input.preset, input.template.permissions?.preset),
    computer_files: false,
    model: input.model,
    name: input.name.trim()
  });
  const patch = (look: Look) => client.patchBot(out.bot.id, { expected_version: out.bot.version, ops: lookOps(look) });
  // A picture the daemon refuses must not cost the shape and colour: try once more without it.
  return patch(input.look)
    .catch(() => (input.look.avatar ? patch({ ...input.look, avatar: null }) : Promise.reject(new Error("look"))))
    .catch(() => out.bot);
}
