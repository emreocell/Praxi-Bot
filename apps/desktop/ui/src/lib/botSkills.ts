import type {
  AvailableSkill,
  BotSkills,
  PutBotSkillsInput,
  SkillAssignmentMode
} from "../api/types.ts";
import { matchesQuery } from "./searchText.ts";

/** Local draft state for the "Choose skills" dialog. */
export interface BotSkillsDraft {
  /** Assignment version the dialog was opened with; sent back as `expected_version`. */
  version: number;
  mode: SkillAssignmentMode;
  selected: Set<string>;
  missing: string[];
  initializedSelected: boolean;
  query: string;
}

/** Server-side cap on assigned names (`MAX_ASSIGNED_SKILLS`). */
export const MAX_ASSIGNED_SKILLS = 200;

/** Summary counts rendered on the bot profile card before opening the modal. */
export interface BotSkillsSummary {
  mode: SkillAssignmentMode;
  availableCount: number;
  assignedCount: number;
  missingCount: number;
  ownCount: number;
}

/** Build the initial modal draft from the server's `GET /v1/bots/{id}/skills` payload. */
export function createBotSkillsDraft(dto: BotSkills): BotSkillsDraft {
  if (dto.mode === "selected") {
    return {
      version: dto.version,
      mode: "selected",
      selected: new Set(dto.assigned.map((a) => a.name)),
      missing: dto.assigned
        .filter((a) => a.state === "missing")
        .map((a) => a.name),
      initializedSelected: true,
      query: ""
    };
  }
  return {
    version: dto.version,
    mode: "all",
    selected: new Set(),
    missing: [],
    initializedSelected: false,
    query: ""
  };
}

/**
 * Switch between `"all"` and `"selected"`.
 *
 * The first switch from `"all"` to `"selected"` pre-checks every currently
 * available shared/bundled skill so enabling filtering never silently strips
 * the bot bare. Subsequent switches preserve the existing draft selection.
 */
export function switchSkillMode(
  draft: BotSkillsDraft,
  nextMode: SkillAssignmentMode,
  available: AvailableSkill[]
): BotSkillsDraft {
  if (nextMode === draft.mode) {
    return draft;
  }
  if (nextMode === "selected" && !draft.initializedSelected) {
    return {
      ...draft,
      mode: "selected",
      selected: new Set(available.map((s) => s.name)),
      initializedSelected: true
    };
  }
  return {
    ...draft,
    mode: nextMode
  };
}

/** Toggle one catalog skill inside the draft selection. */
export function toggleDraftSkill(
  draft: BotSkillsDraft,
  name: string,
  checked: boolean
): BotSkillsDraft {
  const selected = new Set(draft.selected);
  if (checked) {
    selected.add(name);
    return { ...draft, selected };
  }
  selected.delete(name);
  return {
    ...draft,
    selected,
    missing: draft.missing.filter((m) => m !== name)
  };
}

/** Remove an unavailable (`missing`) assignment entry from the draft. */
export function removeMissingDraftSkill(
  draft: BotSkillsDraft,
  name: string
): BotSkillsDraft {
  if (!draft.selected.has(name) && !draft.missing.includes(name)) {
    return draft;
  }
  const selected = new Set(draft.selected);
  selected.delete(name);
  return {
    ...draft,
    selected,
    missing: draft.missing.filter((m) => m !== name)
  };
}

/** Filter catalog skills by search query (case- and Turkish-diacritic-insensitive). */
export function filterAvailableSkills(
  available: AvailableSkill[],
  query: string
): AvailableSkill[] {
  return available.filter((s) => matchesQuery(query, s.name, s.description));
}

/** Build the `PUT /v1/bots/{id}/skills` request body from the current draft. */
export function buildPutSkillsPayload(
  draft: BotSkillsDraft,
  available: AvailableSkill[]
): PutBotSkillsInput {
  if (draft.mode === "all") {
    return { mode: "all", expected_version: draft.version };
  }
  const names: string[] = [];
  const seen = new Set<string>();
  for (const s of available) {
    if (draft.selected.has(s.name) && !seen.has(s.name)) {
      seen.add(s.name);
      names.push(s.name);
    }
  }
  for (const m of draft.missing) {
    if (draft.selected.has(m) && !seen.has(m)) {
      seen.add(m);
      names.push(m);
    }
  }
  return { mode: "selected", names, expected_version: draft.version };
}

/** True when a `selected` payload has more names than the daemon accepts. */
export function exceedsSkillLimit(payload: PutBotSkillsInput): boolean {
  return (payload.names?.length ?? 0) > MAX_ASSIGNED_SKILLS;
}

/** Compute non-zero summary numbers for the bot profile skills card. */
export function summarizeBotSkills(dto: BotSkills): BotSkillsSummary {
  const availableCount = dto.available.length;
  const missingCount = dto.assigned.filter((a) => a.state === "missing").length;
  const assignedCount =
    dto.mode === "all"
      ? availableCount
      : dto.assigned.filter((a) => a.state === "ok").length;
  return {
    mode: dto.mode,
    availableCount,
    assignedCount,
    missingCount,
    ownCount: dto.own.length
  };
}

/** Whether the draft differs from the server payload. */
export function isBotSkillsDirty(draft: BotSkillsDraft, dto: BotSkills): boolean {
  if (draft.mode !== dto.mode) {
    return true;
  }
  if (draft.mode === "all") {
    return false;
  }
  const orig = new Set(dto.assigned.map((a) => a.name));
  if (orig.size !== draft.selected.size) {
    return true;
  }
  for (const name of draft.selected) {
    if (!orig.has(name)) {
      return true;
    }
  }
  return false;
}

/** How a failed `PUT /v1/bots/{id}/skills` is presented. */
export type SkillsSaveFailure = "conflict" | "invalid" | "error";

/** `409`: someone else saved first (reload, never overwrite); `422`: the daemon refused the names. */
export function skillsSaveFailure(status: number | null): SkillsSaveFailure {
  if (status === 409) return "conflict";
  if (status === 422) return "invalid";
  return "error";
}

/**
 * Give one more catalog skill to a bot: nothing to send while the bot uses every shared skill,
 * otherwise the current names plus the new one.
 */
export function payloadWithSkill(dto: BotSkills, name: string): PutBotSkillsInput | null {
  if (dto.mode === "all") {
    return null;
  }
  const names = dto.assigned.map((a) => a.name);
  if (names.includes(name)) {
    return null;
  }
  return { mode: "selected", names: [...names, name], expected_version: dto.version };
}

/** One line of the Library tab's skills list. */
export interface PanelSkillRow {
  name: string;
  /** `shared`: from the catalog and visible to the bot; `learned`: the bot's own; `missing`: assigned but gone from disk. */
  kind: "shared" | "learned" | "missing";
}

/** Everything the bot has right now, one row per skill: catalog skills first, then learned, then missing. */
export function panelSkillRows(dto: BotSkills): PanelSkillRow[] {
  const shared = dto.available.filter((s) => s.assigned).map((s): PanelSkillRow => ({ name: s.name, kind: "shared" }));
  const seen = new Set(shared.map((r) => r.name));
  const learned = dto.own.filter((o) => !seen.has(o.name)).map((o): PanelSkillRow => ({ name: o.name, kind: "learned" }));
  const missing = dto.assigned.filter((a) => a.state === "missing").map((a): PanelSkillRow => ({ name: a.name, kind: "missing" }));
  return [...shared, ...learned, ...missing];
}

/** Names the panel previews: assigned skills first by catalog order, capped, plus the rest count. */
export function skillPreview(
  dto: BotSkills,
  limit: number
): { names: string[]; more: number } {
  const active = dto.available.filter((s) => s.assigned).map((s) => s.name);
  const all = [...active, ...dto.own.map((o) => o.name).filter((n) => !active.includes(n))];
  return { names: all.slice(0, limit), more: Math.max(0, all.length - limit) };
}

function baseNames(dto: BotSkills, draftMode: SkillAssignmentMode): Set<string> {
  if (dto.mode === "selected") return new Set(dto.assigned.map((x) => x.name));
  // "All shared skills": a draft switched to "selected" starts from every available skill.
  return draftMode === "selected" ? new Set(dto.available.map((x) => x.name)) : new Set();
}

/**
 * Keep an open editor's draft in step with the server.  A first load, or a new assignment version
 * while nothing is edited, replaces the picks.  With unsaved picks (`previous` is what they were
 * edited against), they are laid over the new assignment instead of being thrown away: what the
 * user added stays added, what they removed stays removed, and whatever changed on the server
 * (a skill assigned from the add flow, someone else's save) is kept.  The search text survives.
 */
export function syncSkillsDraft(
  current: BotSkillsDraft | null,
  dto: BotSkills,
  previous: BotSkills | null = null
): BotSkillsDraft {
  if (current && current.version === dto.version) {
    return current;
  }
  const fresh = { ...createBotSkillsDraft(dto), query: current?.query ?? "" };
  if (!current || !previous || !isBotSkillsDirty(current, previous)) {
    return fresh;
  }
  const before = baseNames(previous, current.mode);
  const added = [...current.selected].filter((n) => !before.has(n));
  const removed = [...before].filter((n) => !current.selected.has(n));
  const selected = baseNames(dto, current.mode);
  for (const n of added) selected.add(n);
  for (const n of removed) selected.delete(n);
  const missing = [
    ...new Set([...current.missing, ...fresh.missing])
  ].filter((n) => selected.has(n));
  return {
    version: dto.version,
    mode: current.mode,
    selected,
    missing,
    initializedSelected: current.initializedSelected || fresh.initializedSelected,
    query: current.query
  };
}
