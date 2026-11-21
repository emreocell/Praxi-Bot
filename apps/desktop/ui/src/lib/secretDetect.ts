/**
 * Detects a pasted credential in the composer BEFORE the message is sent (issue #421).
 *
 * The daemon-side detector (`secret_store::detect_secrets`, docs/design/secret-detection.md) has
 * no route or Tauri command yet, so this is a deliberate, small port of its HIGH-confidence rule
 * only: provider formats with a fixed prefix and length. It does not port the labelled-context
 * rule (`password: X`), so it misses more than the Rust detector does; it must never report more
 * (no entropy guessing), because a detector that cries wolf gets switched off.
 *
 * Offer, never act: nothing here stores, removes or sends anything. A candidate holds only a
 * `kind` and byte-free character offsets, never the matched text, so printing one cannot leak it.
 */
export interface SecretCandidate {
  kind: string;
  /** UTF-16 offsets into the scanned string. */
  start: number;
  end: number;
}

// Word edges: a longer run of key characters around a shape disqualifies it (`task-...` is not `sk-...`).
const B = "(?<![A-Za-z0-9])";
const SHAPES: ReadonlyArray<{ kind: string; re: RegExp; ok?: (m: string) => boolean }> = [
  { kind: "openai_style_key", re: new RegExp(`${B}sk-[A-Za-z0-9_-]{16,}(?![A-Za-z0-9_-])`, "g"), ok: (m) => /[0-9]/.test(m) },
  { kind: "google_api_key", re: new RegExp(`${B}AIza[0-9A-Za-z_-]{30,}(?![A-Za-z0-9_-])`, "g") },
  { kind: "github_token", re: new RegExp(`${B}(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{20,})(?![A-Za-z0-9_])`, "g") },
  { kind: "slack_token", re: new RegExp(`${B}xox[abprs]-[A-Za-z0-9-]{10,}`, "g") },
  { kind: "nvidia_key", re: new RegExp(`${B}nvapi-[A-Za-z0-9_-]{20,}`, "g") },
  { kind: "jwt", re: new RegExp(`${B}eyJ[A-Za-z0-9_-]+\\.eyJ[A-Za-z0-9_-]+\\.[A-Za-z0-9_-]+`, "g") },
  { kind: "aws_access_key_id", re: new RegExp(`${B}(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Za-z0-9])`, "g") },
  { kind: "stripe_key", re: new RegExp(`${B}[sr]k_live_[A-Za-z0-9]{20,}`, "g") },
  { kind: "telegram_bot_token", re: new RegExp(`${B}[0-9]{8,10}:[A-Za-z0-9_-]{35}(?![A-Za-z0-9_-])`, "g"), ok: (m) => /[A-Z]/.test(m.split(":")[1] ?? "") },
  { kind: "npm_token", re: new RegExp(`${B}npm_[A-Za-z0-9]{36}(?![A-Za-z0-9])`, "g") },
  { kind: "gitlab_token", re: new RegExp(`${B}glpat-[A-Za-z0-9_-]{20,}`, "g") },
  { kind: "huggingface_token", re: new RegExp(`${B}hf_[A-Za-z]{34}(?![A-Za-z])`, "g") },
  { kind: "groq_key", re: new RegExp(`${B}gsk_[A-Za-z0-9]{40,}`, "g") },
  { kind: "digitalocean_token", re: new RegExp(`${B}dop_v1_[0-9a-f]{64}(?![0-9a-f])`, "g") },
  {
    kind: "private_key",
    re: /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)/g
  }
];

/** Candidates in text order, without overlaps (an earlier, longer match wins). */
export function detectSecrets(text: string): SecretCandidate[] {
  const found: SecretCandidate[] = [];
  for (const { kind, re, ok } of SHAPES) {
    re.lastIndex = 0;
    for (const m of text.matchAll(re)) {
      if (ok && !ok(m[0])) continue;
      found.push({ kind, start: m.index ?? 0, end: (m.index ?? 0) + m[0].length });
    }
  }
  found.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: SecretCandidate[] = [];
  for (const c of found) {
    if (out.length === 0 || c.start >= out[out.length - 1].end) out.push(c);
  }
  return out;
}

/** `[secret: name]`, the plain-text stand-in for a stored value (it carries a name, never a value). */
export function placeholder(name: string): string {
  return `[secret: ${name}]`;
}

/** The vault's name rule (the daemon checks again): letters, digits `.` `_` `-` and `/`, at most 64 bytes. */
export function isValidSecretName(name: string): boolean {
  return (
    name.length > 0 &&
    new TextEncoder().encode(name).length <= 64 &&
    /^[A-Za-z0-9._-]+(\/[A-Za-z0-9._-]+)*$/.test(name) &&
    !name.includes("__") &&
    !name.split("/").some((seg) => /^\.+$/.test(seg)) &&
    !/^(composio|mcp)\//i.test(name)
  );
}

/** A starting name for the vault entry, from the kind (the owner can change it). */
export function suggestedName(kind: string): string {
  return `pasted/${kind.replace(/_/g, "-")}`;
}

/** `text` without the candidates' spans. */
export function removeSecrets(text: string, found: readonly SecretCandidate[]): string {
  return rewrite(text, found, () => "");
}

/**
 * Store plan: the values to put in the vault (entry name, value) and the text with each span
 * replaced by a placeholder. With several candidates the names get `-2`, `-3` ... after the first.
 * The returned `text` never contains a value; `entries` does, so it must go straight to the
 * vault call and be dropped.
 */
export function planStore(
  text: string,
  found: readonly SecretCandidate[],
  name: string
): { entries: Array<{ name: string; value: string }>; text: string } {
  const entries = found.map((c, i) => ({
    name: i === 0 ? name : `${name}-${i + 1}`,
    value: text.slice(c.start, c.end)
  }));
  return { entries, text: rewrite(text, found, (_c, i) => placeholder(entries[i].name)) };
}

function rewrite(
  text: string,
  found: readonly SecretCandidate[],
  by: (c: SecretCandidate, i: number) => string
): string {
  let out = "";
  let at = 0;
  found.forEach((c, i) => {
    out += text.slice(at, c.start) + by(c, i);
    at = c.end;
  });
  return out + text.slice(at);
}
