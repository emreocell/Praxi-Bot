/**
 * A first-run checklist shows one obvious next step: the first step that is not done. Later steps
 * stay visible as a short list (so the owner sees how far it is) but carry no button yet.
 */
export type SetupStepState = "done" | "next" | "later";

export function stepStates(done: readonly boolean[]): SetupStepState[] {
  const next = done.findIndex((d) => !d);
  return done.map((d, i) => (d ? "done" : i === next ? "next" : "later"));
}
