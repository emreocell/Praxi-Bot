/** Address a caller uses. The daemon already includes no extra port; this is the existing local API. */
export function webhookAddress(baseUrl: string, id: string): string {
  const base = baseUrl.replace(/\/$/, "");
  return `${base}/v1/incoming-webhooks/${id}`;
}

/**
 * The exact text the signature covers: the timestamp, a dot, then the raw body.
 * Callers sign this with the webhook secret. The body is not a tool call.
 */
export function signedMaterial(timestampMs: string, body: string): string {
  return `${timestampMs}.${body}`;
}
