import type { BotShapeId } from "ui/bot-shape";
import type { Bot, BotPatchInput, PermissionPreset } from "../api/types.ts";
import { ACCESS_LEVELS } from "./accessLevels.ts";
import { AvatarFileError, avatarProblem } from "./avatar.ts";
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
 * The ceiling sent with a template import: the page's level, never above what the template asks
 * for (the daemon clamps too, but the client does not rely on it).  The page offers only the four
 * levels; anything else (a stale "custom") falls back to where the template starts, never to "full".
 */
export function importLevel(preset: PresetChoice, requested: string | null | undefined): SharePreset {
  if (!isSharePreset(preset)) return initialLevel("template", isSharePreset(requested) ? requested : undefined);
  const top = templateTop(requested);
  return STRENGTH[preset] <= STRENGTH[top] ? preset : top;
}

/**
 * The levels a choice can really get, most freedom first.  All four for the general helper; for a
 * template none above what it asks for, because the import grants the lower of the two.
 */
export function levelsFor(choice: string, requested: string | null | undefined): PermissionPreset[] {
  const top = choice === GENERAL_CHOICE ? STRENGTH.full : STRENGTH[templateTop(requested)];
  return ACCESS_LEVELS.filter((l) => STRENGTH[l] <= top).sort((a, b) => STRENGTH[b] - STRENGTH[a]);
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
  return choice !== GENERAL_CHOICE && model.trim().toLowerCase().startsWith("cli:");
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

/** The daemon calls a template needs (a structural slice of the API client, so tests can fake it). */
export interface TemplateClient {
  importProfile(input: ImportInput): Promise<ImportOutput>;
  patchBot(id: string, input: BotPatchInput): Promise<Bot>;
}

/** The daemon refused the picture itself (not the shape or colour): wrong type, too big, not base64. */
export function isAvatarRefusal(e: unknown): boolean {
  const err = e as { status?: number; message?: string } | null;
  return !!err && (err.status === 400 || err.status === 413 || err.status === 422) && /avatar/i.test(err.message ?? "");
}

/**
 * Give a bot the chosen look.  Only a refused picture is retried without it (the shape and colour
 * must not be lost to it); any other failure (a version conflict, the service down) is thrown.
 */
export async function applyLook(client: Pick<TemplateClient, "patchBot">, bot: Bot, look: Look): Promise<Bot> {
  const patch = (l: Look) => client.patchBot(bot.id, { expected_version: bot.version, ops: lookOps(l) });
  try {
    return await patch(look);
  } catch (e) {
    if (!look.avatar || !isAvatarRefusal(e)) throw e;
    return patch({ ...look, avatar: null });
  }
}

/** The template bot exists but its look could not be applied: `bot` is the bot to retry on, `reason` why. */
export class LookNotAppliedError extends Error {
  readonly bot: Bot;
  readonly reason: unknown;
  constructor(bot: Bot, reason: unknown) {
    super("look not applied");
    this.bot = bot;
    this.reason = reason;
  }
}

/**
 * Create a bot from a gallery template the way the review view does: an import the daemon clamps to
 * the chosen level (the client never asks for more than the template does), with the owner's model
 * and name and no file access yet; then the chosen look.  A picture that is not a valid avatar is
 * refused before anything is created (`AvatarFileError`); a look the daemon then refuses is a
 * `LookNotAppliedError`, never reported as success.
 * The model must not be a command-line engine (see `templateNeedsOtherModel`).
 */
export async function createFromTemplate(
  client: TemplateClient,
  input: { template: SharedProfile; preset: PresetChoice; model: string; name: string; look: Look }
): Promise<Bot> {
  const problem = input.look.avatar ? avatarProblem(input.look.avatar) : null;
  if (problem) throw new AvatarFileError(problem);
  const out = await client.importProfile({
    contents: JSON.stringify(input.template),
    permission_level: importLevel(input.preset, input.template.permissions?.preset),
    computer_files: false,
    model: input.model,
    name: input.name.trim()
  });
  try {
    return await applyLook(client, out.bot, input.look);
  } catch (e) {
    throw new LookNotAppliedError(out.bot, e);
  }
}
