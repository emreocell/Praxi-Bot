import type { AutonomyLevel } from "./botProfile.ts";

/**
 * The New-bot dialog as three short steps: how to start (blank, a ready-made bot or a file), the
 * name and look, then what it can do (its AI, the autonomy level and the advanced settings) with
 * Create.  Pure, so the order and the rules are tested without a browser.
 */
export type NewBotStep = "start" | "identity" | "access";

export const NEW_BOT_STEPS: readonly NewBotStep[] = ["start", "identity", "access"];

/** 1-based, for "Step 2 of 3". */
export function stepNumber(step: NewBotStep): number {
  return NEW_BOT_STEPS.indexOf(step) + 1;
}

export function nextStep(step: NewBotStep): NewBotStep | null {
  return NEW_BOT_STEPS[NEW_BOT_STEPS.indexOf(step) + 1] ?? null;
}

export function prevStep(step: NewBotStep): NewBotStep | null {
  const i = NEW_BOT_STEPS.indexOf(step);
  return i > 0 ? (NEW_BOT_STEPS[i - 1] ?? null) : null;
}

/**
 * Whether the stepper may jump from `current` to `target`: always back; forward only to the step
 * right after a complete one (the name step needs a name).  The first step is a choice, so it is
 * left with its cards, never skipped from the stepper.
 */
export function canGoTo(current: NewBotStep, target: NewBotStep, state: { nameMissing: boolean }): boolean {
  const from = NEW_BOT_STEPS.indexOf(current);
  const to = NEW_BOT_STEPS.indexOf(target);
  if (to < from) return true;
  if (to !== from + 1) return false;
  return current === "identity" && !state.nameMissing;
}

export type GalleryLevel = { kind: "autonomy"; level: AutonomyLevel } | { kind: "preset"; preset: "read-only" };

/**
 * How freely a ready-made bot starts, for its card.  Gallery templates are imported at most at
 * "approve-destructive" (`initialLevel` in profileShare.ts), so "full" reads as Balanced here.
 */
export function galleryLevel(preset: string | null | undefined): GalleryLevel | null {
  switch (preset) {
    case "read-and-draft":
      return { kind: "autonomy", level: "ask" };
    case "approve-destructive":
    case "full":
      return { kind: "autonomy", level: "balanced" };
    case "read-only":
      return { kind: "preset", preset: "read-only" };
    default:
      return null;
  }
}
