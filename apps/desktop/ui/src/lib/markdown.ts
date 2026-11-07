/**
 * A small, dependency-free Markdown parser for assistant messages.
 *
 * Safe by construction: it produces a plain AST that React renders as elements
 * and text nodes. Raw HTML is never interpreted (`<script>` is just text) and
 * link targets are restricted to http(s) / mailto by `sanitizeUrl`.
 *
 * Supported: headings, paragraphs, fenced code, blockquotes, (nested, task)
 * lists, GFM tables, rules, and inline code / bold / italic / strike / links /
 * bare URLs. Unclosed constructs (a streaming message) degrade gracefully.
 */

export type Inline =
  | { t: "text"; text: string }
  | { t: "code"; text: string }
  | { t: "strong"; children: Inline[] }
  | { t: "em"; children: Inline[] }
  | { t: "del"; children: Inline[] }
  | { t: "link"; href: string; children: Inline[] }
  | { t: "br" };

export type Align = "left" | "center" | "right" | null;

export type Block =
  | { t: "p"; children: Inline[] }
  | { t: "heading"; level: number; children: Inline[] }
  | { t: "code"; lang: string; text: string }
  | { t: "quote"; children: Block[] }
  | { t: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { t: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: "hr" };

export interface ListItem {
  /** `null` = a plain item; otherwise a GFM task checkbox. */
  checked: boolean | null;
  children: Block[];
}

const MAX_DEPTH = 6;

/** Only http(s) and mailto links are ever clickable. Returns `null` otherwise. */
export function sanitizeUrl(raw: string): string | null {
  // Browsers ignore control characters and whitespace inside the scheme
  // (`java\tscript:`); strip them before checking.
  // eslint-disable-next-line no-control-regex
  const url = raw.replace(/[\u0000-\u001f\u007f\s]+/g, "");
  if (!url || url.length > 2048) {
    return null;
  }
  return /^(https?:\/\/|mailto:)[^\s]+$/i.test(url) ? url : null;
}

// ------------------------------------------------------------------ inline

function isAlnum(ch: string | undefined): boolean {
  return ch !== undefined && /[\p{L}\p{N}]/u.test(ch);
}

function isSpace(ch: string | undefined): boolean {
  return ch === undefined || /\s/.test(ch);
}

/** Index of the `]` that closes the `[` at `open`, or -1. */
function closeBracket(s: string, open: number): number {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (c === "\\") {
      i++;
    } else if (c === "[") {
      depth++;
    } else if (c === "]") {
      depth--;
      if (depth === 0) {
        return i;
      }
    }
  }
  return -1;
}

export function parseInline(s: string, depth = 0): Inline[] {
  const out: Inline[] = [];
  let buf = "";
  const flush = () => {
    if (buf) {
      out.push({ t: "text", text: buf });
      buf = "";
    }
  };
  const nest = (inner: string): Inline[] =>
    depth >= MAX_DEPTH
      ? [{ t: "text", text: inner }]
      : parseInline(inner, depth + 1);

  let i = 0;
  while (i < s.length) {
    const c = s[i] ?? "";

    if (
      c === "\\" &&
      i + 1 < s.length &&
      /[\\`*_{}[\]()#+\-.!|~<>]/.test(s[i + 1] ?? "")
    ) {
      buf += s[i + 1];
      i += 2;
      continue;
    }
    if (c === "\n") {
      flush();
      out.push({ t: "br" });
      i++;
      continue;
    }
    if (c === "`") {
      let n = 1;
      while (s[i + n] === "`") {
        n++;
      }
      const fence = "`".repeat(n);
      const end = s.indexOf(fence, i + n);
      if (end !== -1 && end > i + n - 1) {
        flush();
        out.push({
          t: "code",
          text: s.slice(i + n, end).replace(/^ (.*) $/, "$1")
        });
        i = end + n;
        continue;
      }
      buf += fence;
      i += n;
      continue;
    }
    if (c === "*" && s[i + 1] === "*" && !isSpace(s[i + 2])) {
      const end = s.indexOf("**", i + 2);
      if (end > i + 2 && !isSpace(s[end - 1])) {
        flush();
        out.push({ t: "strong", children: nest(s.slice(i + 2, end)) });
        i = end + 2;
        continue;
      }
    }
    if (c === "~" && s[i + 1] === "~" && !isSpace(s[i + 2])) {
      const end = s.indexOf("~~", i + 2);
      if (end > i + 2 && !isSpace(s[end - 1])) {
        flush();
        out.push({ t: "del", children: nest(s.slice(i + 2, end)) });
        i = end + 2;
        continue;
      }
    }
    if ((c === "*" || c === "_") && s[i + 1] !== c && !isSpace(s[i + 1])) {
      const intraword = c === "_" && isAlnum(s[i - 1]);
      if (!intraword) {
        let from = i + 1;
        let found = -1;
        for (;;) {
          const end = s.indexOf(c, from);
          if (end === -1) {
            break;
          }
          if (
            s[end + 1] !== c &&
            !isSpace(s[end - 1]) &&
            (c === "*" || !isAlnum(s[end + 1]))
          ) {
            found = end;
            break;
          }
          from = end + 1;
        }
        if (found > i + 1) {
          flush();
          out.push({ t: "em", children: nest(s.slice(i + 1, found)) });
          i = found + 1;
          continue;
        }
      }
    }
    if (c === "[") {
      const close = closeBracket(s, i);
      if (close > i && s[close + 1] === "(") {
        let depthP = 0;
        let end = -1;
        for (let j = close + 1; j < s.length; j++) {
          if (s[j] === "(") {
            depthP++;
          } else if (s[j] === ")") {
            depthP--;
            if (depthP === 0) {
              end = j;
              break;
            }
          }
        }
        if (end !== -1) {
          // `[text](url "title")`: the title is dropped
          const target =
            s.slice(close + 2, end).trim().split(/\s+/)[0] ?? "";
          const href = sanitizeUrl(target.replace(/^<|>$/g, ""));
          flush();
          const label = nest(s.slice(i + 1, close));
          if (href) {
            out.push({ t: "link", href, children: label });
          } else {
            // unsafe or unsupported target: keep the label as plain text
            out.push(...label);
          }
          i = end + 1;
          continue;
        }
      }
    }
    if (c === "h" && !isAlnum(s[i - 1])) {
      const m = /^https?:\/\/[^\s<>]+/i.exec(s.slice(i));
      if (m) {
        let url = m[0];
        // trailing punctuation belongs to the sentence; so does an unbalanced ")"
        for (;;) {
          const last = url[url.length - 1] ?? "";
          const unbalanced =
            last === ")" &&
            (url.match(/\)/g) ?? []).length > (url.match(/\(/g) ?? []).length;
          if (/[.,;:!?'"\]]/.test(last) || unbalanced) {
            url = url.slice(0, -1);
          } else {
            break;
          }
        }
        const href = sanitizeUrl(url);
        if (href && url.length > 8) {
          flush();
          out.push({ t: "link", href, children: [{ t: "text", text: url }] });
          i += url.length;
          continue;
        }
      }
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}

// ------------------------------------------------------------------ blocks

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)[^`]*$/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const HR = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const LIST_ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_DELIM = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) {
    s = s.slice(1);
  }
  if (s.endsWith("|") && !s.endsWith("\\|")) {
    s = s.slice(0, -1);
  }
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
    } else {
      cur += s[i];
    }
  }
  cells.push(cur.trim());
  return cells;
}

function alignOf(cell: string): Align {
  const l = cell.startsWith(":");
  const r = cell.endsWith(":");
  return l && r ? "center" : r ? "right" : l ? "left" : null;
}

function startsBlock(line: string): boolean {
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    HR.test(line) ||
    /^ {0,3}>/.test(line) ||
    LIST_ITEM.test(line)
  );
}

function isTableStart(line: string, next: string | undefined): boolean {
  return (
    line.includes("|") &&
    next !== undefined &&
    next.includes("-") &&
    TABLE_DELIM.test(next)
  );
}

function plainPara(text: string): Block[] {
  return [{ t: "p", children: [{ t: "text", text }] }];
}

export function parseMarkdown(src: string, depth = 0): Block[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.trim() === "") {
      i++;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1] ?? "```";
      const body: string[] = [];
      i++;
      // an unclosed fence (still streaming) runs to the end
      while (i < lines.length) {
        const l = (lines[i] ?? "").trim();
        if (l.startsWith(marker) && l.replace(/[`~]/g, "") === "") {
          i++;
          break;
        }
        body.push(lines[i] ?? "");
        i++;
      }
      blocks.push({
        t: "code",
        lang: (fence[2] ?? "").toLowerCase(),
        text: body.join("\n")
      });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      blocks.push({
        t: "heading",
        level: (heading[1] ?? "#").length,
        children: parseInline(heading[2] ?? "")
      });
      i++;
      continue;
    }

    if (HR.test(line)) {
      blocks.push({ t: "hr" });
      i++;
      continue;
    }

    if (/^ {0,3}>/.test(line)) {
      const body: string[] = [];
      while (i < lines.length && /^ {0,3}>/.test(lines[i] ?? "")) {
        body.push((lines[i] ?? "").replace(/^ {0,3}> ?/, ""));
        i++;
      }
      const text = body.join("\n");
      blocks.push({
        t: "quote",
        children:
          depth >= MAX_DEPTH ? plainPara(text) : parseMarkdown(text, depth + 1)
      });
      continue;
    }

    if (isTableStart(line, lines[i + 1])) {
      const head = splitRow(line);
      const delim = splitRow(lines[i + 1] ?? "");
      if (head.length === delim.length) {
        const rows: Inline[][][] = [];
        i += 2;
        while (
          i < lines.length &&
          (lines[i] ?? "").trim() !== "" &&
          (lines[i] ?? "").includes("|")
        ) {
          const cells = splitRow(lines[i] ?? "");
          // ragged rows are padded / cut to the header width
          rows.push(head.map((_, c) => parseInline(cells[c] ?? "", 1)));
          i++;
        }
        blocks.push({
          t: "table",
          align: delim.map(alignOf),
          head: head.map((h) => parseInline(h, 1)),
          rows
        });
        continue;
      }
    }

    const li = LIST_ITEM.exec(line);
    if (li) {
      const ordered = /\d/.test(li[2] ?? "");
      const start = ordered ? parseInt(li[2] ?? "1", 10) : 1;
      const baseIndent = (li[1] ?? "").length;
      const items: ListItem[] = [];
      while (i < lines.length) {
        const m = LIST_ITEM.exec(lines[i] ?? "");
        if (
          !m ||
          (m[1] ?? "").length !== baseIndent ||
          /\d/.test(m[2] ?? "") !== ordered
        ) {
          break;
        }
        const contentIndent = baseIndent + (m[2] ?? "").length + 1;
        const body: string[] = [m[3] ?? ""];
        i++;
        while (i < lines.length) {
          const l = lines[i] ?? "";
          if (l.trim() === "") {
            // a blank line continues the item only when an indented line follows
            const next = lines[i + 1] ?? "";
            if (next.trim() !== "" && next.search(/\S/) >= contentIndent) {
              body.push("");
              i++;
              continue;
            }
            break;
          }
          const ind = l.search(/\S/);
          if (ind >= contentIndent || (ind > baseIndent && LIST_ITEM.test(l))) {
            body.push(l.slice(Math.min(ind, contentIndent)));
            i++;
          } else if (ind <= baseIndent && !startsBlock(l)) {
            // lazy continuation of the paragraph
            body.push(l.trim());
            i++;
          } else {
            break;
          }
        }
        let text = body.join("\n");
        let checked: boolean | null = null;
        const task = /^\[([ xX])\]\s+/.exec(text);
        if (task) {
          checked = task[1] !== " ";
          text = text.slice(task[0].length);
        }
        items.push({
          checked,
          children:
            depth >= MAX_DEPTH ? plainPara(text) : parseMarkdown(text, depth + 1)
        });
        // blank lines between items keep the list going
        let j = i;
        while (j < lines.length && (lines[j] ?? "").trim() === "") {
          j++;
        }
        const nm = LIST_ITEM.exec(lines[j] ?? "");
        if (
          nm &&
          (nm[1] ?? "").length === baseIndent &&
          /\d/.test(nm[2] ?? "") === ordered
        ) {
          i = j;
        }
      }
      blocks.push({ t: "list", ordered, start, items });
      continue;
    }

    // paragraph: runs until a blank line or another block starts
    const para: string[] = [line];
    i++;
    while (i < lines.length) {
      const l = lines[i] ?? "";
      if (l.trim() === "" || startsBlock(l) || isTableStart(l, lines[i + 1])) {
        break;
      }
      para.push(l);
      i++;
    }
    blocks.push({
      t: "p",
      children: parseInline(para.map((p) => p.trim()).join("\n"))
    });
  }
  return blocks;
}
