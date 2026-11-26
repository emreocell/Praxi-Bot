/**
 * "Connected as <account>": the proof that a connected app works.
 *
 * Right after an app becomes active (and whenever the owner presses Test) the daemon makes one
 * fixed read-only call and answers with the account and up to three samples (Drive file names,
 * Gmail subjects, calendar names, the GitHub user). This file is the small state machine for that
 * answer and the rule for when it runs by itself. It never decides whether an app is connected:
 * that is the app list's state.
 */

import type { ComposioApp, ComposioTest } from "../api/types.ts";

/**
 * Apps the daemon knows a safe test for (the daemon's `probe.rs`; it refuses any other app with a
 * plain sentence, so a stale entry here only shows a button that explains itself).
 */
export const TESTABLE_APPS: readonly string[] = ["googledrive", "gmail", "googlecalendar", "github"];

export function isTestable(slug: string): boolean {
  return TESTABLE_APPS.includes(slug);
}

/** What the test of one app shows. */
export type AppTestState =
  | { status: "running" }
  | { status: "ok"; account: string | null; items: string[] }
  | { status: "failed"; title: string; hint: string; details: string };

export interface TestFailure {
  title: string;
  hint: string;
  details: string;
}

export interface TestDeps {
  test(slug: string): Promise<ComposioTest>;
  /** Word an error for the person (the human-error layer in the app). */
  describe(error: unknown): TestFailure;
}

/** Run one test and report each state: `running`, then `ok` or `failed`. Never throws. */
export async function runAppTest(
  deps: TestDeps,
  slug: string,
  report: (slug: string, state: AppTestState) => void
): Promise<AppTestState> {
  report(slug, { status: "running" });
  let end: AppTestState;
  try {
    const found = await deps.test(slug);
    end = {
      status: "ok",
      account: found.account?.trim() ? found.account : null,
      items: found.items.filter((i) => i.trim() !== "").slice(0, 3)
    };
  } catch (e) {
    const f = deps.describe(e);
    end = { status: "failed", title: f.title, hint: f.hint, details: f.details };
  }
  report(slug, end);
  return end;
}

/**
 * The apps that just became active (they were waiting for sign-in in `before`) and have no test
 * yet: those are tested by themselves, so "Connected as ..." appears without another click.
 * An app that was already active when the list first loaded is not tested again behind the
 * owner's back; the Test button is there for that.
 */
export function appsToTestNow(
  before: readonly ComposioApp[],
  after: readonly ComposioApp[],
  tests: Readonly<Record<string, AppTestState>>
): string[] {
  return after
    .filter(
      (a) =>
        a.state === "active" &&
        a.enabled &&
        isTestable(a.slug) &&
        tests[a.slug] === undefined &&
        before.some((b) => b.slug === a.slug && b.state === "pending")
    )
    .map((a) => a.slug);
}
