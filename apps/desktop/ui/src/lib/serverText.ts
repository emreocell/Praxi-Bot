import { escapeInvisible } from "./approvalPlain.ts";

/** A stretch of text from an MCP server. `external` parts look like an address that leads off this computer. */
export interface ServerTextPart {
  text: string;
  external: boolean;
  /** An address written with letters outside ASCII, which can imitate a well-known site. */
  unusual: boolean;
}

// A scheme address (https://, praxi://, ...), mailto:, www., or a bare domain with an optional path.
const ADDRESS =
  /(?:[a-z][a-z0-9+.-]*:\/\/|mailto:|www\.)[^\s<>"'`]+|(?:[\p{L}\p{N}][\p{L}\p{N}-]*\.)+\p{L}{2,}(?::\d+)?(?:\/[^\s<>"'`]*)?/giu;
const TRAILING = /[.,;:!?)\]}»”’]+$/u;

/**
 * Server text split into plain pieces and address-like pieces, with invisible and bidirectional
 * control characters written out. The pieces are meant to be shown as text nodes: nothing here
 * becomes a link, and nothing is interpreted as HTML.
 */
export function splitServerText(raw: string): ServerTextPart[] {
  const text = escapeInvisible(raw);
  const parts: ServerTextPart[] = [];
  let at = 0;
  const plain = (end: number) => {
    if (end > at) parts.push({ text: text.slice(at, end), external: false, unusual: false });
  };
  for (const match of text.matchAll(ADDRESS)) {
    const address = match[0].replace(TRAILING, "");
    if (!address) continue;
    plain(match.index);
    parts.push({ text: address, external: true, unusual: /\P{ASCII}/u.test(address) });
    at = match.index + address.length;
  }
  plain(text.length);
  return parts;
}

/** Whether any part of the text looks like an external address. */
export function hasExternalAddress(parts: readonly ServerTextPart[]): boolean {
  return parts.some((part) => part.external);
}
