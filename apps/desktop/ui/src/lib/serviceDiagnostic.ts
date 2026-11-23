import type { StartupDiagnostics } from "../api/connection.ts";
import { redactDetails } from "./redactDetails.ts";

/**
 * Plain text the owner can paste into a bug report when the local service does not come back.
 * Built from what the app shell knows (no messages, no keys); the user folder is masked.
 */
export function buildServiceDiagnostic(d: StartupDiagnostics | null, phase: string): string {
  const lines = [`app phase: ${phase}`];
  if (d) {
    lines.push(
      `app version: ${d.app_version}`,
      `os: ${d.os}`,
      `start stage: ${d.stage}`,
      `port: ${d.port}`,
      `service file present: ${d.sidecar_exists ? "yes" : "no"}`,
      `exit code: ${d.exit_code ?? "none"}`,
      `error: ${d.error ?? "none"}`,
      "last log lines:",
      ...d.log_tail.slice(-20)
    );
  } else {
    lines.push("no start-up details available");
  }
  return redactDetails(lines.join("\n"));
}
