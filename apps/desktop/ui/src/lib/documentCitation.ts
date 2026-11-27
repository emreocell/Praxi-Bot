/**
 * Citations in answers about the owner's documents.
 *
 * `documents_search` gives every passage a markdown link such as
 * `[lease.pdf, p. 3](praxi-doc://<source id>/housing/lease.pdf?lines=12-19&page=3)`. The model copies
 * it into its answer; the chat shows it as a button that opens that file at that page. The target is
 * only a pointer: the daemon opens a file only if it is indexed from an enabled folder of this bot.
 */
import type { ApiClient } from "../api/client";

export const CITATION_SCHEME = "praxi-doc:";

export interface DocCitation {
  sourceId: string;
  /** Path of the file inside the chosen folder, `/`-separated. */
  locator: string;
  fileName: string;
  /** One-based PDF page, when the citation names one. */
  page: number | null;
  /** Lines of the indexed text (the original lines for Markdown and plain text). */
  lines: { from: number; to: number } | null;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;

/** Read a `praxi-doc://` link; `null` for anything else or anything malformed. */
export function parseCitationHref(href: string): DocCitation | null {
  if (!href.toLowerCase().startsWith(`${CITATION_SCHEME}//`) || href.length > 2048) return null;
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  if (url.protocol !== CITATION_SCHEME || !UUID.test(url.hostname) || url.username || url.password || url.hash) return null;
  // The raw text, not `url.pathname`: the URL parser would quietly resolve `..` segments.
  const rawPath = href.replace(/^[^:]+:\/\/[^/?#]*/, "").split(/[?#]/)[0];
  const segments: string[] = [];
  for (const raw of rawPath.split("/").slice(1)) {
    let part: string;
    try {
      part = decodeURIComponent(raw);
    } catch {
      return null;
    }
    if (!part || part === "." || part === ".." || part.includes("\\") || CONTROL.test(part)) return null;
    segments.push(part);
  }
  if (segments.length === 0) return null;
  const number = (value: string | null): number | null => {
    if (value === null || !/^\d{1,9}$/.test(value)) return null;
    const n = Number(value);
    return n >= 1 ? n : null;
  };
  const range = /^(\d{1,9})-(\d{1,9})$/.exec(url.searchParams.get("lines") ?? "");
  const from = range ? Number(range[1]) : 0;
  const to = range ? Number(range[2]) : 0;
  return {
    sourceId: url.hostname.toLowerCase(),
    locator: segments.join("/"),
    fileName: segments[segments.length - 1],
    page: number(url.searchParams.get("page")),
    lines: range && from >= 1 && to >= from ? { from, to } : null
  };
}

/** Whether a browser window can show this file itself (everything else is saved as a copy). */
export function canViewInWindow(fileName: string): boolean {
  return /\.pdf$/i.test(fileName);
}

/** Link target for a PDF blob that opens on the cited page. */
export function viewerUrl(blobUrl: string, page: number | null): string {
  return page ? `${blobUrl}#page=${page}` : blobUrl;
}

export type OpenResult = "viewed" | "saved";

/**
 * Open the file behind a citation. A PDF opens in a new window at its page when the shell allows it;
 * anything else (or a blocked window) is saved as a copy, which the owner then opens normally.
 */
export async function openCitation(
  client: Pick<ApiClient, "getKnowledgeFile">,
  botId: string,
  citation: DocCitation,
  env: { open: (url: string) => boolean; save: (url: string, name: string) => void; revokeLater: (url: string) => void } = browserEnv()
): Promise<OpenResult> {
  const viewable = canViewInWindow(citation.fileName);
  const blob = await client.getKnowledgeFile(botId, citation.sourceId, citation.locator, !viewable);
  const url = URL.createObjectURL(viewable ? new Blob([blob], { type: "application/pdf" }) : blob);
  try {
    if (viewable && env.open(viewerUrl(url, citation.page))) return "viewed";
    env.save(url, citation.fileName);
    return "saved";
  } finally {
    // The viewer window keeps reading the blob for a while.
    env.revokeLater(url);
  }
}

function browserEnv() {
  return {
    open: (url: string) => window.open(url, "_blank", "noopener") !== null,
    save: (url: string, name: string) => {
      const a = document.createElement("a");
      a.href = url;
      a.download = name;
      a.click();
    },
    revokeLater: (url: string) => {
      window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
    }
  };
}
