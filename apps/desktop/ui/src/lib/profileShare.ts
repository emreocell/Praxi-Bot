import type { components } from "../api/schema";

type S = components["schemas"];
export type SharedProfile = S["SharedProfile"];
export type ImportPreview = S["ImportPreview"];
export type ImportInput = S["ImportInput"];
export type ImportOutput = S["ImportOutput"];
export type ExportOutput = S["ExportOutput"];
export type SharePreset = S["SharePreset"];
export type PreviewNote = S["PreviewNote"];

/** Least to most permissive: the importer picks a ceiling from these. */
export const SHARE_LEVELS: readonly SharePreset[] = [
  "read-only",
  "read-and-draft",
  "approve-destructive",
  "full"
] as const;

/** Largest profile file the daemon accepts (bytes). */
export const MAX_PROFILE_BYTES = 64 * 1024;

type Lang = "en" | "tr" | "de";

function pick(map: Record<string, string> | undefined, lang: string): string | undefined {
  if (!map) return undefined;
  const short = lang.split("-")[0] as Lang;
  return map[short] || map.en || Object.values(map)[0];
}

/** Gallery card title in the UI language (falls back to English, then the bot name). */
export function templateName(p: SharedProfile, lang: string): string {
  return pick(p.meta?.names, lang) ?? p.bot.name;
}

export function templateDescription(p: SharedProfile, lang: string): string {
  return pick(p.meta?.descriptions, lang) ?? "";
}

/**
 * Where the ceiling starts. A profile file from someone else starts at the safest level; the
 * built-in gallery (reviewed, shipped with the app) starts at what the template asks for, never
 * higher than "approve-destructive".
 */
export function initialLevel(source: "file" | "template", requested?: SharePreset): SharePreset {
  if (source === "file") return "read-only";
  return requested && requested !== "full" ? requested : "approve-destructive";
}

/** Notes that mean something was lowered or left out, shown as a warning rather than info. */
export function noteIsWarning(code: string): boolean {
  return code === "level_lowered" || code === "rule_lowered";
}

/** Save text as a file through the webview's download (no path is ever sent to the daemon). */
export function saveTextFile(filename: string, text: string): void {
  const blob = new Blob([text], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
