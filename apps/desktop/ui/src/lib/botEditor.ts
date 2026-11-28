import { PERMISSION_CATEGORIES } from "../api/types.ts";
import {
  type BotDraft,
  formatModelRef,
  type ModelRef,
  parseModelRef,
  type PresetChoice
} from "./botProfile.ts";

/**
 * State rules of the "Edit bot" dialog (owner feedback 244, point 5): its sections, what counts
 * as unsaved, what blocks saving, and what closing does.  The dialog component only renders.
 */

/**
 * The editor is one scrolling page; these are its sections, top to bottom.  Skills live in the
 * "apps" section (what the bot may use); voice is part of "identity".
 */
export const BOT_EDITOR_SECTIONS = [
  "identity",
  "instructions",
  "models",
  "place",
  "permissions",
  "apps",
  "memory",
  "limits",
  "advanced"
] as const;

export type BotEditorSection = (typeof BOT_EDITOR_SECTIONS)[number];

/** A section name from outside (a review URL): only known sections pass. */
export function editorSection(value: string | null | undefined): BotEditorSection | undefined {
  return BOT_EDITOR_SECTIONS.find((s) => s === value);
}

/**
 * Sections that do not wait for the page's Save: memory saves the moment it changes, spending
 * limits have their own Save button.  The page says so next to their title, and Cancel does not
 * undo them.
 */
export const SEPARATE_SAVE: Partial<Record<BotEditorSection, "onChange" | "ownButton">> = {
  memory: "onChange",
  limits: "ownButton"
};

/**
 * The section the reader is looking at while scrolling the page: the last one whose top has
 * passed the top of the viewport (plus a small margin); the last section once the page is
 * scrolled to the end, since a short last section can never reach the top.
 */
export function sectionAtScroll(
  tops: readonly number[],
  scroll: { top: number; height: number; scrollHeight: number },
  margin = 32
): number {
  if (tops.length === 0) return 0;
  if (scroll.top + scroll.height >= scroll.scrollHeight - 2) return tops.length - 1;
  let at = 0;
  tops.forEach((top, i) => {
    if (top <= scroll.top + margin) at = i;
  });
  return at;
}

function sameRef(a: ModelRef, b: ModelRef): boolean {
  return formatModelRef(a) === formatModelRef(b);
}

export interface EditorSnapshot {
  draft: BotDraft;
  initialDraft: BotDraft;
  apps: Record<string, boolean>;
  initialApps: Record<string, boolean>;
  skillsDirty: boolean;
  /** Hand-typed model ids that are not valid yet (kept as typed, see `invalidCustomRoles`). */
  customText?: CustomText;
  /** Text typed in the "Other model id" fallback field that was not added yet. */
  pendingFallback?: string;
}

function appsChanged(a: Record<string, boolean>, b: Record<string, boolean>): boolean {
  const names = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const name of names) {
    if ((a[name] ?? false) !== (b[name] ?? false)) return true;
  }
  return false;
}

/** Which sections hold unsaved changes. */
export function dirtySections(s: EditorSnapshot): BotEditorSection[] {
  const { draft: d, initialDraft: i } = s;
  const out: BotEditorSection[] = [];
  if (
    d.name.trim() !== i.name.trim() ||
    d.shape !== i.shape ||
    d.color.toLowerCase() !== i.color.toLowerCase() ||
    (d.avatar ?? null) !== (i.avatar ?? null)
  ) {
    out.push("identity");
  }
  if (d.persona !== i.persona) out.push("instructions");
  if (
    !sameRef(d.executor, i.executor) ||
    !sameRef(d.planner, i.planner) ||
    !sameRef(d.classifier, i.classifier) ||
    fallbackList(d.fallback).join(",") !== fallbackList(i.fallback).join(",") ||
    (s.pendingFallback ?? "").trim() !== ""
  ) {
    out.push("models");
  }
  if (d.preset !== i.preset || PERMISSION_CATEGORIES.some((c) => d.categories[c] !== i.categories[c])) {
    out.push("permissions");
  }
  if (s.skillsDirty || appsChanged(s.apps, s.initialApps)) out.push("apps");
  if (invalidCustomRoles(s.customText ?? {}).length > 0) out.push("advanced");
  return out;
}

export type EditorProblem =
  | "nameMissing"
  | "executorMissing"
  | "customInvalid"
  | "fallbackPending";

/** What blocks Save, and the section that fixes it. */
export function editorProblems(
  draft: BotDraft,
  customText: CustomText = {},
  pendingFallback = ""
): Array<{ problem: EditorProblem; section: BotEditorSection }> {
  const out: Array<{ problem: EditorProblem; section: BotEditorSection }> = [];
  if (draft.name.trim() === "") out.push({ problem: "nameMissing", section: "identity" });
  if (draft.executor.model.trim() === "") {
    out.push({ problem: "executorMissing", section: "models" });
  }
  if (invalidCustomRoles(customText).length > 0) {
    out.push({ problem: "customInvalid", section: "advanced" });
  }
  // Typed but not added (valid or not): Save would silently leave it out.
  if (pendingFallback.trim() !== "") {
    out.push({ problem: "fallbackPending", section: "models" });
  }
  return out;
}

/**
 * Closing the dialog (Esc, backdrop, the X, Cancel): never while a save is in flight, and only
 * after a confirmation when something is unsaved.
 */
export function closeAction(dirty: boolean, saving: boolean): "close" | "confirm" | "wait" {
  if (saving) return "wait";
  return dirty ? "confirm" : "close";
}

// --- fallback models -------------------------------------------------------

/** The stored comma list as ordered, de-duplicated refs. */
export function fallbackList(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of text.split(",")) {
    const ref = part.trim();
    if (ref && !seen.has(ref)) {
      seen.add(ref);
      out.push(ref);
    }
  }
  return out;
}

export function addFallback(text: string, ref: string): string {
  return fallbackList(`${text},${ref}`).join(", ");
}

export function removeFallback(text: string, ref: string): string {
  return fallbackList(text)
    .filter((r) => r !== ref)
    .join(", ");
}

// --- custom model id (the "Advanced" escape hatch) -------------------------

export type CustomRefIssue = "empty" | "spaces" | "noModel";

/**
 * Check a hand-typed `provider/model` id.  The model part may itself contain slashes
 * (`openrouter/meta-llama/llama-3.3`); a missing provider is allowed (default provider).
 */
export function customRefIssue(text: string): CustomRefIssue | null {
  const t = text.trim();
  if (!t) return "empty";
  if (/\s/.test(t)) return "spaces";
  const ref = parseModelRef(t);
  if (!ref.model.trim()) return "noModel";
  return null;
}

/** A typed id as the `ModelRef` a draft stores; `null` while it is not usable. */
export function customRef(text: string): ModelRef | null {
  return customRefIssue(text) ? null : parseModelRef(text.trim());
}

// --- permission mode chip --------------------------------------------------

export type PermissionTone = "locked" | "guarded" | "open" | "custom";

/** How the permission chip is tinted: what the bot may do without asking. */
export function permissionTone(preset: PresetChoice): PermissionTone {
  switch (preset) {
    case "read-only":
      return "locked";
    case "full":
      return "open";
    case "custom":
      return "custom";
    case "approve-destructive":
    case "read-and-draft":
      return "guarded";
  }
}

/** Roles with a hand-typed model id (Advanced). */
export type CustomRole = "executor" | "planner" | "classifier";

/** The text of a custom id field while it differs from the draft's model (kept as typed). */
export type CustomText = Partial<Record<CustomRole, string>>;

/**
 * Typed ids that are not usable: they block Save and count as unsaved data.  An empty field is
 * not "invalid" here (an optional role may be cleared; an empty main model is `executorMissing`).
 */
export function invalidCustomRoles(text: CustomText): CustomRole[] {
  return (Object.keys(text) as CustomRole[]).filter((role) => {
    const raw = text[role];
    return raw !== undefined && raw.trim() !== "" && customRef(raw) === null;
  });
}

/**
 * One keystroke in a custom id field: the draft value to store (`null` = keep the draft as it is
 * while the text is invalid) and the text to keep (`undefined` = the draft's own text is shown).
 */
export function applyCustomText(
  text: string
): { ref: ModelRef | null; keep: string | undefined } {
  const ref = text.trim() === "" ? { provider: "", model: "" } : customRef(text);
  const canonical = ref ? formatModelRef(ref) : undefined;
  return { ref, keep: ref && text === canonical ? undefined : text };
}

// --- fallback entry ----------------------------------------------------------

export type FallbackIssue = CustomRefIssue;

/**
 * Check a typed fallback list (`provider/model`, comma separated, any id - not only the catalog's).
 * Returns the refs to add, or the first issue.
 */
export function parseFallbackInput(
  text: string
): { refs: string[] } | { issue: CustomRefIssue } {
  const parts = text.split(",").map((p) => p.trim()).filter(Boolean);
  if (parts.length === 0) return { issue: "empty" };
  for (const part of parts) {
    const issue = customRefIssue(part);
    if (issue) return { issue };
  }
  return { refs: parts };
}
