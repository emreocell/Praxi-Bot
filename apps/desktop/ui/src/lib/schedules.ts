import type { Schedule, ScheduleInput } from "../api/types";
import type { RunState } from "../state/timeline.ts";

/** `ScheduleDto::spec`: when a schedule fires. */
export type ScheduleSpec =
  | { cron: string }
  | { every: { seconds: number } }
  | { at: { unix_ms: number } };

/** `ScheduleDto::payload`: what a schedule does. */
export type SchedulePayload =
  | { kind: "agent_prompt"; text: string }
  | { kind: "workflow"; name: string; params?: unknown }
  | { kind: "tool"; name: string; input?: unknown }
  | { kind: "unknown" };

export type MisfirePolicy = "skip" | "run_once" | "run_all";
export const MISFIRE_POLICIES: readonly MisfirePolicy[] = [
  "skip",
  "run_once",
  "run_all"
] as const;

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

/** Read the (untrusted) `spec` JSON, or `null` for a shape this UI does not know. */
export function parseSpec(raw: unknown): ScheduleSpec | null {
  if (!isObj(raw)) {
    return null;
  }
  if (typeof raw.cron === "string") {
    return { cron: raw.cron };
  }
  if (isObj(raw.every) && typeof raw.every.seconds === "number") {
    return { every: { seconds: raw.every.seconds } };
  }
  if (isObj(raw.at) && typeof raw.at.unix_ms === "number") {
    return { at: { unix_ms: raw.at.unix_ms } };
  }
  return null;
}

/** Read the (untrusted) `payload` JSON. */
export function parsePayload(raw: unknown): SchedulePayload {
  if (!isObj(raw)) {
    return { kind: "unknown" };
  }
  if (raw.kind === "agent_prompt" && typeof raw.text === "string") {
    return { kind: "agent_prompt", text: raw.text };
  }
  if (raw.kind === "workflow" && typeof raw.name === "string") {
    return { kind: "workflow", name: raw.name, params: raw.params };
  }
  if (raw.kind === "tool" && typeof raw.name === "string") {
    return { kind: "tool", name: raw.name, input: raw.input };
  }
  return { kind: "unknown" };
}

/** Picks the largest unit that divides `seconds` exactly. */
export function intervalParts(seconds: number): {
  unit: "days" | "hours" | "minutes" | "seconds";
  value: number;
} {
  if (seconds > 0 && seconds % 86400 === 0) {
    return { unit: "days", value: seconds / 86400 };
  }
  if (seconds > 0 && seconds % 3600 === 0) {
    return { unit: "hours", value: seconds / 3600 };
  }
  if (seconds > 0 && seconds % 60 === 0) {
    return { unit: "minutes", value: seconds / 60 };
  }
  return { unit: "seconds", value: seconds };
}

export interface SpecLabel {
  /** i18n key (`schedules.spec.*`) and its values. */
  i18nKey: string;
  values: Record<string, string | number>;
}

/** The label of a spec: `cron`, `every` (with unit), `at` or `unknown`. */
export function specLabel(
  raw: unknown,
  formatDate: (ms: number) => string
): SpecLabel {
  const spec = parseSpec(raw);
  if (!spec) {
    return { i18nKey: "schedules.spec.unknown", values: {} };
  }
  if ("cron" in spec) {
    return { i18nKey: "schedules.spec.cron", values: { cron: spec.cron } };
  }
  if ("every" in spec) {
    const p = intervalParts(spec.every.seconds);
    return {
      i18nKey: `schedules.spec.every.${p.unit}`,
      values: { count: p.value }
    };
  }
  return {
    i18nKey: "schedules.spec.at",
    values: { when: formatDate(spec.at.unix_ms) }
  };
}

export type ScheduleStatus = "enabled" | "disabled" | "finished";

/** A disabled schedule is paused; an enabled one without a next firing is done. */
export function scheduleStatus(
  s: Pick<Schedule, "enabled" | "next_run_at_ms">
): ScheduleStatus {
  if (!s.enabled) {
    return "disabled";
  }
  return s.next_run_at_ms === null || s.next_run_at_ms === undefined
    ? "finished"
    : "enabled";
}

/** Soonest next firing first, schedules without one last; ties by name. */
export function sortSchedules(list: readonly Schedule[]): Schedule[] {
  return [...list].sort((a, b) => {
    const an = a.enabled ? (a.next_run_at_ms ?? Infinity) : Infinity;
    const bn = b.enabled ? (b.next_run_at_ms ?? Infinity) : Infinity;
    if (an !== bn) {
      return an < bn ? -1 : 1;
    }
    return a.name.localeCompare(b.name);
  });
}

/** Recent runs started by schedule `id` (newest first), from the event state. */
export function runsOfSchedule(
  runs: Record<string, RunState>,
  id: string,
  limit = 5
): RunState[] {
  return Object.values(runs)
    .filter((r) => r.scheduleId === id && r.parentRunId === undefined)
    .sort((a, b) => b.startedMs - a.startedMs)
    .slice(0, limit);
}

export type RunOutcome = "running" | "succeeded" | "failed";

export function runOutcome(
  r: Pick<RunState, "finishedMs" | "error">
): RunOutcome {
  if (r.finishedMs === undefined) {
    return "running";
  }
  return r.error ? "failed" : "succeeded";
}

/** Whether a daemon event means the schedule list may have changed. */
export function affectsSchedules(event: {
  type: string;
  schedule_id?: string | null;
}): boolean {
  return (
    (event.type === "run_started" && !!event.schedule_id) ||
    event.type === "run_finished"
  );
}

export type ScheduleKind = "natural" | "cron" | "keep";

export interface ScheduleDraft {
  name: string;
  botId: string;
  /**
   * `natural` = phrase parsed by the daemon, `cron` = raw expression,
   * `keep` = edit without changing the stored `spec`.
   */
  kind: ScheduleKind;
  text: string;
  cron: string;
  prompt: string;
  enabled: boolean;
  misfire: MisfirePolicy;
}

export function emptyDraft(botId: string): ScheduleDraft {
  return {
    name: "",
    botId,
    kind: "natural",
    text: "",
    cron: "",
    prompt: "",
    enabled: true,
    misfire: "skip"
  };
}

export function draftFromSchedule(s: Schedule): ScheduleDraft {
  const spec = parseSpec(s.spec);
  const payload = parsePayload(s.payload);
  const misfire =
    MISFIRE_POLICIES.find((m) => m === s.misfire_policy) ?? "skip";
  return {
    name: s.name,
    botId: s.bot_id,
    kind: spec && "cron" in spec ? "cron" : "keep",
    text: "",
    cron: spec && "cron" in spec ? spec.cron : "",
    prompt: payload.kind === "agent_prompt" ? payload.text : "",
    enabled: s.enabled,
    misfire
  };
}

export type DraftError = "name" | "bot" | "text" | "cron" | "prompt";

/** Whether an edit keeps the stored payload (a workflow / tool payload is not editable here). */
function keepsPayload(editing?: Schedule): boolean {
  return (
    editing !== undefined && parsePayload(editing.payload).kind !== "agent_prompt"
  );
}

/** The first problem of a draft, or `null`. */
export function validateDraft(
  d: ScheduleDraft,
  editing?: Schedule
): DraftError | null {
  if (d.name.trim() === "") {
    return "name";
  }
  if (d.botId === "") {
    return "bot";
  }
  if (d.kind === "natural" && d.text.trim() === "") {
    return "text";
  }
  if (d.kind === "cron" && d.cron.trim() === "") {
    return "cron";
  }
  if (!keepsPayload(editing) && d.prompt.trim() === "") {
    return "prompt";
  }
  return null;
}

/**
 * The request body for a draft.  `spec` is the resolved spec of the `natural`
 * kind (from the parse call); it throws when it is missing.
 */
export function buildInput(
  d: ScheduleDraft,
  spec: unknown,
  editing?: Schedule
): ScheduleInput {
  let resolved: unknown;
  switch (d.kind) {
    case "natural":
      if (spec === null || spec === undefined) {
        throw new Error("natural-language schedule was not parsed");
      }
      resolved = spec;
      break;
    case "cron":
      resolved = { cron: d.cron.trim() };
      break;
    case "keep":
      if (!editing) {
        throw new Error("nothing to keep");
      }
      resolved = editing.spec;
      break;
  }
  const payload =
    editing && keepsPayload(editing)
      ? editing.payload
      : { kind: "agent_prompt", text: d.prompt.trim() };
  return {
    bot_id: d.botId,
    name: d.name.trim(),
    spec: resolved,
    payload,
    enabled: d.enabled,
    misfire_policy: d.misfire,
    // keep an existing chat on edit; a new schedule gets the daemon default
    chat_id: editing?.chat_id ?? null
  };
}

/** The same schedule with `enabled` flipped, as a `PUT` body. */
export function toggleInput(s: Schedule): ScheduleInput {
  return {
    bot_id: s.bot_id,
    chat_id: s.chat_id ?? null,
    name: s.name,
    spec: s.spec,
    payload: s.payload,
    enabled: !s.enabled,
    misfire_policy: s.misfire_policy
  };
}
