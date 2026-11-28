/**
 * Finds web addresses in untrusted text so the UI can show them as inert text. A link inside a
 * server's message is never opened by Praxi, and never made clickable.
 */
export interface LinkSegment {
  kind: "text" | "url";
  text: string;
}

// http(s), ftp-like schemes, and a bare `www.`. The lookbehind keeps lookalikes such as
// "awww.example.com", "sub.www.example.com" or "xhttp://x" out. One linear scan: no nested quantifiers.
const URL_AT = /(?<![\p{L}\p{N}_.@/-])(?:(?:https?|ftps?|sftp):\/\/|www\.)[^\s<>"'`\u00a0]+/giu;
const SCHEME = /^(?:(?:https?|ftps?|sftp):\/\/|www\.)/iu;
const PAIRS: Record<string, string> = { ")": "(", "]": "[", "}": "{" };

/** How many trailing characters are sentence punctuation or an unmatched closing bracket. */
function trailingJunk(url: string): number {
  let end = url.length;
  while (end > 0) {
    const ch = url[end - 1];
    if (".,;:!?'\"\u2019\u201d\u00bb\u2026".includes(ch)) {
      end -= 1;
    } else if (ch in PAIRS) {
      let open = 0;
      let close = 0;
      for (let i = 0; i < end; i += 1) {
        if (url[i] === PAIRS[ch]) open += 1;
        else if (url[i] === ch) close += 1;
      }
      if (close > open) end -= 1;
      else break;
    } else {
      break;
    }
  }
  return url.length - end;
}

/** Split `text` into plain and URL segments; joining the segments gives `text` back exactly. */
export function splitExternalLinks(text: string): LinkSegment[] {
  const out: LinkSegment[] = [];
  let last = 0;
  const push = (kind: LinkSegment["kind"], s: string) => {
    if (s === "") return;
    const prev = out[out.length - 1];
    if (prev && prev.kind === kind) prev.text += s;
    else out.push({ kind, text: s });
  };
  for (const m of text.matchAll(URL_AT)) {
    const start = m.index ?? 0;
    const raw = m[0];
    const url = raw.slice(0, raw.length - trailingJunk(raw));
    // Nothing after the scheme ("http://." or "www.)"): not an address.
    if (url.replace(SCHEME, "") === "") continue;
    push("text", text.slice(last, start));
    push("url", url);
    last = start + url.length;
  }
  push("text", text.slice(last));
  return out;
}
