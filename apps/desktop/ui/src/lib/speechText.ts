/**
 * Turn a Markdown chat reply into text that reads well aloud (ADR 0021): code blocks and tables
 * are summarised instead of spelled out, links keep only their words, and formatting marks vanish.
 * Pure, so the rules are tested without a browser or a speech engine.
 */

export interface SpeechLabels {
  /** Said in place of a fenced code block. */
  code: string;
  /** Said in place of a table. */
  table: string;
}

const TABLE_ROW = /^\s*\|.*\|\s*$/;

/** Collapse fenced code and pipe tables to one spoken label each. */
function summariseBlocks(lines: string[], labels: SpeechLabels): string[] {
  const out: string[] = [];
  let inFence = false;
  let inTable = false;
  for (const line of lines) {
    if (/^\s*(```|~~~)/.test(line)) {
      if (!inFence) {
        out.push(labels.code);
      }
      inFence = !inFence;
      inTable = false;
      continue;
    }
    if (inFence) {
      continue;
    }
    if (TABLE_ROW.test(line)) {
      if (!inTable) {
        out.push(labels.table);
      }
      inTable = true;
      continue;
    }
    inTable = false;
    out.push(line);
  }
  return out;
}

export function markdownToSpeech(markdown: string, labels: SpeechLabels): string {
  const lines = summariseBlocks(markdown.replace(/\r\n?/g, "\n").split("\n"), labels);
  const text = lines
    .map((line) =>
      line
        .replace(/^\s{0,3}#{1,6}\s+/, "")
        .replace(/^\s*>\s?/, "")
        .replace(/^\s*(?:[-*+]|\d+[.)])\s+/, "")
        .replace(/^\s*([-*_])(\s*\1){2,}\s*$/, "")
    )
    .join("\n")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/<\/?[a-zA-Z][^>\n]*>/g, "")
    .replace(/https?:\/\/\S+/g, "")
    .replace(/(\*\*|__|~~)(.+?)\1/g, "$2")
    .replace(/(^|[\s(])[*_]([^*_\n]+)[*_](?=[\s).,!?;:]|$)/g, "$1$2")
    .replace(/`([^`\n]*)`/g, "$1")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{2,}/g, "\n")
    .replace(/[ \t]{2,}/g, " ");
  return text.trim();
}
