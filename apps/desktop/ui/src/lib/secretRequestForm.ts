/**
 * Pure rules of the credential-request card (`SecretRequestCard`), kept apart from React so they
 * can be tested. Nothing here stores, logs or returns a value other than the body the card posts.
 */
export type SecretFields = "secret" | "login";

export interface SubmitBody {
  value?: string;
  username?: string;
  password?: string;
  origins: string[];
  programs: string[];
}

/** Entries of an allow-list box: one per line, comma or space; trimmed, no repeats. */
export function parseAllowList(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[\s,]+/)) {
    const v = part.trim();
    if (v !== "" && !out.includes(v)) out.push(v);
  }
  return out;
}

/** The body for `POST /v1/secret-requests/{call_id}`, or `null` while a required field is empty. */
export function buildSubmitBody(
  fields: SecretFields,
  input: { value?: string; username?: string; password?: string },
  origins: string[],
  programs: string[]
): SubmitBody | null {
  if (fields === "secret") {
    return input.value ? { value: input.value, origins, programs } : null;
  }
  return input.username && input.password
    ? { username: input.username, password: input.password, origins, programs }
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
