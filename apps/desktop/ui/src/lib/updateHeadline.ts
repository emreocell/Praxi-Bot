import type { UpdateInfo, UpdateStatus } from "./appUpdate.ts";

/** The one state the Updates summary card announces. */
export type UpdateHeadline = "notSetUp" | "available" | "upToDate" | "unchecked";

/**
 * A build without a signed update feed is "not set up" whatever else is known; a found update wins
 * over "up to date"; a check that found nothing, or any earlier check, reads as up to date.
 */
export function updateHeadline(
  status: Pick<UpdateStatus, "enabled" | "lastChecked">,
  found: UpdateInfo | null,
  upToDate: boolean
): UpdateHeadline {
  if (!status.enabled) return "notSetUp";
  if (found) return "available";
  if (upToDate || status.lastChecked !== null) return "upToDate";
  return "unchecked";
}
