/** Error codes the shell can return; anything else counts as "failed". */
const KNOWN_CODES = [
  "not_configured",
  "preview_not_configured",
  "network",
  "no_release",
  "signature",
  "busy",
  "nothing_to_install",
  "no_rollback",
  "failed"
] as const;

/** i18n key (under settingsModal.updates.errors) for a shell error. */
export function updateErrorKey(err: unknown): string {
  const text = typeof err === "string" ? err : err instanceof Error ? err.message : "";
  const code = KNOWN_CODES.find((c) => c === text);
  return `settingsModal.updates.errors.${code ?? "failed"}`;
}
