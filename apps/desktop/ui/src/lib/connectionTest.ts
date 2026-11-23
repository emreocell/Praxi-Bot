/** Same base as the Help window's "Read more" links (`DOCS_BASE_URL` in `helpFaq.ts`). */
const DOCS_BASE_URL = "https://github.com/emreocell/Praxi-Bot/blob/main/docs/guide/";

/** What a "Test connection" run can find; mirrors the daemon's `ConnectionOutcome`. */
export const CONNECTION_OUTCOMES = [
  "ok",
  "bad_key",
  "no_network",
  "quota_exceeded",
  "wrong_endpoint",
  "provider_outage",
  "unknown"
] as const;

export type ConnectionOutcome = (typeof CONNECTION_OUTCOMES)[number];

/** The pieces of a daemon report the UI needs (the generated type has the same shape). */
export interface ConnectionReportLike {
  target: string;
  outcome: string;
  http_status?: number | null;
  provider_code?: string | null;
  details: string;
}

/** Anything the daemon might add later is shown as "unknown" rather than breaking the screen. */
export function normalizeOutcome(value: string): ConnectionOutcome {
  return (CONNECTION_OUTCOMES as readonly string[]).includes(value) ? (value as ConnectionOutcome) : "unknown";
}

/** The guide page (in the user's language when translated) and the section for one outcome. */
export function connectionDocsUrl(language: string, outcome: ConnectionOutcome): string {
  const lang = language.slice(0, 2).toLowerCase();
  const folder = lang === "tr" || lang === "de" ? `${lang}/` : "";
  const anchor = outcome === "ok" ? "" : `#${outcome.replace(/_/g, "-")}`;
  return `${DOCS_BASE_URL}${folder}connection-test.md${anchor}`;
}

/** The text for the expandable "details": status and code first, then what the service said. */
export function detailsText(report: ConnectionReportLike): string {
  const lines: string[] = [];
  if (report.http_status != null) {
    lines.push(`HTTP ${report.http_status}`);
  }
  if (report.provider_code) {
    lines.push(`code: ${report.provider_code}`);
  }
  if (report.details) {
    lines.push(report.details);
  }
  return lines.join("\n");
}
