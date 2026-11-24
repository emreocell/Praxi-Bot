import de from "../help/gettingStarted.de.json";
import en from "../help/gettingStarted.en.json";
import tr from "../help/gettingStarted.tr.json";

/** The screen a step's button opens; the shell maps each one to an existing place. */
export type StepTarget = "ai" | "chat" | "apps" | "schedules" | "computer";

interface StepDef {
  id: StepId;
  target: StepTarget;
  /** Optional steps never block the "all done" moment. */
  optional?: boolean;
}

export type StepId = "model" | "message" | "approval" | "app" | "schedule" | "server";

/** The steps in order. The wording lives in src/help/gettingStarted.<lang>.json. */
export const STEP_DEFS: readonly StepDef[] = [
  { id: "model", target: "ai" },
  { id: "message", target: "chat" },
  { id: "approval", target: "chat" },
  { id: "app", target: "apps" },
  { id: "schedule", target: "schedules" },
  { id: "server", target: "computer", optional: true }
];

export interface StepText {
  id: string;
  title: string;
  body: string;
  action: string;
}

const BY_LANGUAGE: Record<string, readonly StepText[]> = { en, tr, de };

/** The step wording in the user's language, English when that language has none. */
export function stepTextFor(language: string): readonly StepText[] {
  return BY_LANGUAGE[language.slice(0, 2).toLowerCase()] ?? en;
}

/** What the app knows right now. `null` = not known (still loading or the call failed). */
export interface Signals {
  hasModel: boolean | null;
  runCount: number | null;
  decidedApprovals: number | null;
  appCount: number | null;
  scheduleCount: number | null;
  serverCount: number | null;
}

export interface Step {
  id: StepId;
  target: StepTarget;
  optional: boolean;
  title: string;
  body: string;
  action: string;
  done: boolean;
}

const positive = (n: number | null): boolean => n !== null && n > 0;

function isDone(id: StepId, s: Signals): boolean {
  switch (id) {
    case "model":
      return s.hasModel === true;
    case "message":
      return positive(s.runCount);
    case "approval":
      return positive(s.decidedApprovals);
    case "app":
      return positive(s.appCount);
    case "schedule":
      return positive(s.scheduleCount);
    case "server":
      return positive(s.serverCount);
  }
}

/** Steps with their text and whether the app's real state says they are done. */
export function buildSteps(language: string, signals: Signals): Step[] {
  const text = stepTextFor(language);
  return STEP_DEFS.map((def) => {
    const t = text.find((x) => x.id === def.id) ?? en.find((x) => x.id === def.id)!;
    return {
      id: def.id,
      target: def.target,
      optional: def.optional === true,
      title: t.title,
      body: t.body,
      action: t.action,
      done: isDone(def.id, signals)
    };
  });
}

export interface Progress {
  done: number;
  total: number;
  /** Every required step is done (optional ones do not count). */
  complete: boolean;
  /** The first step still open, which the panel highlights. */
  next: StepId | null;
}

export function progressOf(steps: readonly Step[]): Progress {
  const required = steps.filter((s) => !s.optional);
  const done = required.filter((s) => s.done).length;
  const next = steps.find((s) => !s.done && !s.optional)?.id ?? null;
  return { done, total: required.length, complete: done === required.length, next };
}

/** Decided approvals only: a pending one does not mean the person has tried the choice yet. */
export function countDecided(approvals: readonly { decision?: string | null }[]): number {
  return approvals.filter((a) => a.decision != null).length;
}
