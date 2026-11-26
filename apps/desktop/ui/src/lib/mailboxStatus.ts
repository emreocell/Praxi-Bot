import type { EmailAccount, EmailTestReport } from "../api/types.ts";
import { nextStep, type MailNextStep } from "./botMail.ts";

/**
 * The one status line of a mailbox card: either "Connected as …" or "Needs attention" with the one
 * thing to do about it. The worst problem wins, so the owner never sees two warnings at once.
 */
export type MailboxStatus =
  | { tone: "ok"; tested: boolean }
  | { tone: "attention"; reason: "secretMissing"; fix: "edit" }
  | { tone: "attention"; reason: "test"; step: Exclude<MailNextStep, "ok">; fix: "edit" | "test" }
  | { tone: "attention"; reason: "toolsOff"; fix: "turnOnTools" };

export function mailboxStatus(
  account: Pick<EmailAccount, "password_set">,
  opts: { report?: EmailTestReport | null; boundBotToolsOn?: boolean | null }
): MailboxStatus {
  if (!account.password_set) {
    return { tone: "attention", reason: "secretMissing", fix: "edit" };
  }
  const report = opts.report ?? null;
  if (report && !report.ok) {
    const step = nextStep(report);
    if (step !== "ok") {
      // Waiting or a dropped connection fixes itself: try again. Anything else needs a changed setting.
      const fix = step === "timeout" || step === "unreachable" ? "test" : "edit";
      return { tone: "attention", reason: "test", step, fix };
    }
  }
  if (opts.boundBotToolsOn === false) {
    return { tone: "attention", reason: "toolsOff", fix: "turnOnTools" };
  }
  return { tone: "ok", tested: report?.ok === true };
}
