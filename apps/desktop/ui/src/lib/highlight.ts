/**
 * A tiny syntax highlighter. It is lazy-loaded (`import()`) the first time a
 * code block renders, so it stays out of the main bundle. It tokenizes into
 * plain data; the React side renders spans, so nothing is ever injected as HTML.
 */

export type TokenType =
  | "plain"
  | "comment"
  | "string"
  | "number"
  | "keyword"
  | "literal";

export interface Token {
  type: TokenType;
  text: string;
}

interface Lang {
  line: string[];
  block?: [string, string];
  quotes: string[];
  keywords: Set<string>;
  literals: Set<string>;
  caseInsensitive?: boolean;
}

const words = (s: string) => new Set(s.split(/\s+/).filter(Boolean));

const JS: Lang = {
  line: ["//"],
  block: ["/*", "*/"],
  quotes: ['"', "'", "`"],
  keywords: words(
    `const let var function return if else for while do switch case break continue
     class extends new import export from default async await try catch finally throw
     typeof instanceof in of interface type enum implements public private protected
     readonly static as void yield delete this super`
  ),
  literals: words("true false null undefined NaN Infinity")
};

const RUST: Lang = {
  line: ["//"],
  block: ["/*", "*/"],
  quotes: ['"'],
  keywords: words(
    `fn let mut const static struct enum impl trait pub use mod crate self Self super
     if else match for while loop break continue return as in where move async await
     dyn ref type unsafe extern`
  ),
  literals: words("true false None Some Ok Err")
};

const PY: Lang = {
  line: ["#"],
  quotes: ['"', "'"],
  keywords: words(
    `def class return if elif else for while break continue import from as try except
     finally raise with lambda pass yield in is not and or global nonlocal async await del assert`
  ),
  literals: words("True False None")
};

const SHELL: Lang = {
  line: ["#"],
  quotes: ['"', "'"],
  keywords: words(
    `if then else elif fi for while do done case esac function in return export local
     echo cd set unset source exit`
  ),
  literals: words("true false")
};

const PWSH: Lang = {
  line: ["#"],
  block: ["<#", "#>"],
  quotes: ['"', "'"],
  keywords: words(
    `if else elseif foreach for while do switch function param return try catch finally
     throw break continue begin process end in`
  ),
  literals: words("true false null $true $false $null"),
  caseInsensitive: true
};

const JSON_LANG: Lang = {
  line: [],
  quotes: ['"'],
  keywords: new Set(),
  literals: words("true false null")
};

const CONFIG: Lang = {
  line: ["#"],
  quotes: ['"', "'"],
  keywords: new Set(),
  literals: words("true false null yes no")
};

const CSS: Lang = {
  line: [],
  block: ["/*", "*/"],
  quotes: ['"', "'"],
  keywords: words("@media @import @keyframes @font-face important"),
  literals: new Set()
};

const SQL: Lang = {
  line: ["--"],
  block: ["/*", "*/"],
  quotes: ["'", '"'],
  keywords: words(
    `select from where insert into values update set delete create table drop alter
     join left right inner outer on group by order having limit as and or not null
     distinct union index primary key`
  ),
  literals: words("true false null"),
  caseInsensitive: true
};

const GENERIC: Lang = {
  line: ["//", "#"],
  block: ["/*", "*/"],
  quotes: ['"', "'"],
  keywords: new Set(),
  literals: words("true false null")
};

const LANGS: Record<string, Lang> = {
  js: JS,
  javascript: JS,
  jsx: JS,
  ts: JS,
  typescript: JS,
  tsx: JS,
  mjs: JS,
  java: JS,
  c: JS,
  cpp: JS,
  cs: JS,
  go: JS,
  rust: RUST,
  rs: RUST,
  python: PY,
  py: PY,
  sh: SHELL,
  bash: SHELL,
  shell: SHELL,
  zsh: SHELL,
  powershell: PWSH,
  pwsh: PWSH,
  ps1: PWSH,
  json: JSON_LANG,
  jsonc: JSON_LANG,
  toml: CONFIG,
  yaml: CONFIG,
  yml: CONFIG,
  ini: CONFIG,
  css: CSS,
  sql: SQL
};

/** Above this the block is rendered plain (keeps streaming responsive). */
export const MAX_HIGHLIGHT_CHARS = 20_000;

export function hasLanguage(lang: string): boolean {
  return lang.toLowerCase() in LANGS;
}

const NUMBER = /^(?:0x[\da-f]+|\d[\d_]*(?:\.\d+)?(?:e[+-]?\d+)?)/i;
const IDENT = /^[\p{L}_$@][\p{L}\p{N}_$-]*/u;

export function highlight(code: string, lang: string): Token[] {
  if (code.length > MAX_HIGHLIGHT_CHARS) {
    return [{ type: "plain", text: code }];
  }
  const spec = LANGS[lang.toLowerCase()] ?? GENERIC;
  const out: Token[] = [];
  const push = (type: TokenType, text: string) => {
    if (!text) {
      return;
    }
    const last = out[out.length - 1];
    if (last && last.type === type) {
      last.text += text;
    } else {
      out.push({ type, text });
    }
  };

  let i = 0;
  while (i < code.length) {
    const rest = code.slice(i);

    if (spec.block && rest.startsWith(spec.block[0])) {
      const end = code.indexOf(spec.block[1], i + spec.block[0].length);
      const stop = end === -1 ? code.length : end + spec.block[1].length;
      push("comment", code.slice(i, stop));
      i = stop;
      continue;
    }
    const lc = spec.line.find((p) => rest.startsWith(p));
    // a `#` inside a word (`a#b`, `$#`) is not a comment
    if (lc && (lc !== "#" || i === 0 || /[\s;]/.test(code.charAt(i - 1)))) {
      const nl = code.indexOf("\n", i);
      const stop = nl === -1 ? code.length : nl;
      push("comment", code.slice(i, stop));
      i = stop;
      continue;
    }
    const q = rest.charAt(0);
    if (spec.quotes.includes(q)) {
      let j = i + 1;
      while (j < code.length) {
        const ch = code.charAt(j);
        if (ch === "\\") {
          j += 2;
          continue;
        }
        j++;
        if (ch === q || (ch === "\n" && q !== "`")) {
          break;
        }
      }
      const stop = Math.min(j, code.length);
      push("string", code.slice(i, stop));
      i = stop;
      continue;
    }
    if (/\d/.test(q) && !/[\p{L}\p{N}_]/u.test(code.charAt(i - 1))) {
      const m = NUMBER.exec(rest);
      if (m) {
        push("number", m[0]);
        i += m[0].length;
        continue;
      }
    }
    const id = IDENT.exec(rest);
    if (id) {
      const w = id[0];
      const key = spec.caseInsensitive ? w.toLowerCase() : w;
      push(
        spec.keywords.has(key)
          ? "keyword"
          : spec.literals.has(key)
            ? "literal"
            : "plain",
        w
      );
      i += w.length;
      continue;
    }
    push("plain", q);
    i++;
  }
  return out;
}
