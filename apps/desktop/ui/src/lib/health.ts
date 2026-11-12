export type HealthLevel = "ok" | "wait" | "bad";

/**
 * What the health strip shows: nothing when healthy on a real provider, only
 * the calm "fake provider" tag when healthy on the fake one (answers are not
 * from a real model), the full strip otherwise.
 */
export function healthStripMode(level: HealthLevel, fakeProvider: boolean): "hidden" | "tag" | "full" {
  if (level !== "ok") return "full";
  return fakeProvider ? "tag" : "hidden";
}
