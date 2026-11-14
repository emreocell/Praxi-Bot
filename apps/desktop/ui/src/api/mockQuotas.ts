import type { AccountQuota } from "../lib/quota.ts";

/** Sample quota data for mock mode only (design review, screenshots). */
export const MOCK_QUOTAS: AccountQuota[] = [
  { id: "cli:claude", brand: "claude", label: "Claude Code", remainingPct: 73 },
  { id: "cli:codex", brand: "openai", label: "Codex", remainingPct: 21 },
  { id: "cli:cursor", brand: "cursor", label: "Cursor", remainingPct: 52 }
];
