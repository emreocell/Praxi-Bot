/**
 * Pure rules of the credential-request card (`SecretRequestCard`), kept apart from React so they
 * can be tested. Nothing here stores, logs or returns a value other than the body the card posts.
 */
/** Thrown instead of returning quietly when there is no daemon connection: the card stays open. */
export class NoClientError extends Error {
  constructor() {
    super("no daemon connection");
    this.name = "NoClientError";
  }
}

export type SecretFields = "secret" | "login";

export interface SubmitBody {
  value?: string;
  username?: string;
  password?: string;
}

/** The daemon's redactor needs at least six bytes. The form requires six characters per field. */
export function buildSubmitBody(
  fields: SecretFields,
  input: { value?: string; username?: string; password?: string }
): SubmitBody | null {
  const filled = (value: string | undefined): value is string => (value?.length ?? 0) >= 6;
  if (fields === "secret") {
    return filled(input.value) ? { value: input.value } : null;
  }
  return filled(input.username) && filled(input.password)
    ? { username: input.username, password: input.password }
    : null;
}

/** Whole seconds until `expiresMs` (never negative). */
export function secondsLeft(expiresMs: number, nowMs: number): number {
  return Math.max(0, Math.ceil((expiresMs - nowMs) / 1000));
}

/** `4:05` for 245 seconds. */
export function formatCountdown(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** Codes after which the card cannot be answered any more. */
export function isGone(code: string): boolean {
  return code === "secret_request_gone";
}
