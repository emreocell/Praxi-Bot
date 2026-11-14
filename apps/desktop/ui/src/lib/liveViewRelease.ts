import type { ControlStateDto, ReleaseRequest } from "../api/types.ts";
import { releaseConfirmedBy } from "./liveViewState.ts";

export interface ReleaseDeps {
  /** `POST /v1/control/release` (through the control bridge when one exists). */
  release(req: ReleaseRequest): Promise<ControlStateDto | null>;
  getControlState(): Promise<ControlStateDto>;
}

export type ReleaseOutcome =
  | { kind: "confirmed"; dto: ControlStateDto }
  | {
      kind: "failed";
      code: string;
      message: string;
      /** Fresh daemon state when it could be read (takeover still reported). */
      dto: ControlStateDto | null;
    };

function describeError(err: unknown, fallbackCode: string) {
  const code =
    err !== null &&
    typeof err === "object" &&
    typeof (err as { code?: unknown }).code === "string"
      ? (err as { code: string }).code
      : fallbackCode;
  const message = err instanceof Error ? err.message : String(err);
  return { code, message };
}

/**
 * Hand a takeover back. The release counts as confirmed only when authoritative
 * daemon state no longer shows that takeover: a rejected POST is verified with a
 * fresh `GET /v1/control/state` (the response may have been lost after the daemon
 * applied it), and anything else is a failure the user can retry with the same
 * request id.
 */
export async function runRelease(
  deps: ReleaseDeps,
  payload: ReleaseRequest
): Promise<ReleaseOutcome> {
  let response: ControlStateDto | null = null;
  let postError: unknown = null;
  try {
    response = await deps.release(payload);
  } catch (err) {
    postError = err;
  }
  if (response && releaseConfirmedBy(response, payload.takeover_id)) {
    return { kind: "confirmed", dto: response };
  }
  let fresh: ControlStateDto | null = null;
  try {
    fresh = await deps.getControlState();
  } catch {
    fresh = null;
  }
  if (fresh && releaseConfirmedBy(fresh, payload.takeover_id)) {
    return { kind: "confirmed", dto: fresh };
  }
  if (postError !== null) {
    return {
      kind: "failed",
      ...describeError(postError, "release_failed"),
      dto: fresh ?? response
    };
  }
  return {
    kind: "failed",
    code: "release_unconfirmed",
    message: "release not confirmed by daemon state",
    dto: fresh ?? response
  };
}
