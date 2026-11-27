import type { PermissionPreset } from "../api/types.ts";
import type { PresetChoice } from "./botProfile.ts";

/**
 * The access levels a bot can have, from the most freedom to the least.  One list for every
 * place that chooses or shows a level (bot editor, new-bot dialog, Settings → Security, the
 * bot panel badge), so the names and their one-line meaning never drift apart.  The texts live
 * under `accessLevel.<preset>` (name, line, can[], stops[]).
 *
 * Owner decision: Full access asks only before spending money and finishes tasks on its own;
 * every lower level asks more.
 */
export const ACCESS_LEVELS: readonly PermissionPreset[] = [
  "full",
  "approve-destructive",
  "read-and-draft",
  "read-only"
] as const;

/** A stored preset as a level the pickers know; anything else (`custom`, unknown, none) is `null`. */
export function accessLevelOf(preset: string | null | undefined): PermissionPreset | null {
  return ACCESS_LEVELS.find((l) => l === preset) ?? null;
}

/** The i18n key prefix of a level (custom rules included). */
export function accessLevelKey(preset: PresetChoice): string {
  return `accessLevel.${preset}`;
}
