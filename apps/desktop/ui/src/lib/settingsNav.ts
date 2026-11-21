import type { SettingsSectionId } from "../views/settings/types";

/** The settings navigation, in groups: who you are, what the bots use, the app itself. */
export const SETTINGS_NAV_GROUPS: ReadonlyArray<{
  id: "personal" | "bots" | "app";
  sections: readonly SettingsSectionId[];
}> = [
  { id: "personal", sections: ["general", "appearance"] },
  { id: "bots", sections: ["ai", "computer", "mail"] },
  { id: "app", sections: ["usage", "diagnostics", "updates"] }
];

/** Every section appears in exactly one group (a new section must be placed). */
export function groupedSections(): SettingsSectionId[] {
  return SETTINGS_NAV_GROUPS.flatMap((g) => [...g.sections]);
}
