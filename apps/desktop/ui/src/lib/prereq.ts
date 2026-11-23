import type { PrereqIssue, PrereqReport } from "../api/connection.ts";

/** The problem to lead with: the first one that blocks the app, else nothing (advice is not a headline). */
export function leadIssue(report: PrereqReport | null): PrereqIssue | null {
  return report?.issues.find((i) => i.blocking) ?? null;
}

/** Issues that are shown only in the Advanced section. */
export function adviceIssues(report: PrereqReport | null): PrereqIssue[] {
  return report?.issues.filter((i) => !i.blocking) ?? [];
}

/** The `errors.prereq.*` key of the primary button for a fix. */
export function fixLabelKey(fix: PrereqIssue["fix"]): string {
  switch (fix) {
    case "download_webview2":
      return "errors.prereq.download";
    case "open_data_folder":
      return "errors.prereq.openDataFolder";
    default:
      return "errors.startupPanel.retry";
  }
}
