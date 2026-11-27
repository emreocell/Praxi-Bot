/**
 * Detects a pasted credential in the composer BEFORE the message is sent (issue #421).
 *
 * A port of the daemon-side detector (`secret_store::detect_secrets`, docs/design/secret-detection.md):
 * provider shapes, private-key blocks, card numbers and labelled values (`password: X`). The
 * desktop has no route or Tauri command for the Rust one, and a draft must not leave the
 * composer, so it runs here, locally. `crates/secret-store/tests/fixtures/detect-corpus.json` is
 * read by both test suites, which is what keeps the two from drifting. Like the Rust one it
 * never guesses from entropy alone: text with no shape and no label is not reported.
 *
 * Offer, never act: nothing here stores, removes or sends anything. A candidate holds only a
 * `kind` and character offsets, never the matched text, so printing one cannot leak it.
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
  // Bounded segments (as long as any real token): `eyJ-eyJ-eyJ-...` has no dot to stop a run, and an
  // unbounded `+` would rescan the rest of the text from every `eyJ`. The Rust shape reads the same minimums.
  { kind: "jwt", re: new RegExp(`${B}eyJ[A-Za-z0-9_-]{8,512}\\.eyJ[A-Za-z0-9_-]{8,8192}\\.[A-Za-z0-9_-]{8,2048}`, "g") },
  { kind: "aws_access_key_id", re: new RegExp(`${B}(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Za-z0-9])`, "g") },
  { kind: "stripe_key", re: new RegExp(`${B}[sr]k_live_[A-Za-z0-9]{20,}`, "g") },
  { kind: "telegram_bot_token", re: new RegExp(`${B}[0-9]{8,10}:[A-Za-z0-9_-]{35}(?![A-Za-z0-9_-])`, "g"), ok: (m) => /[A-Z]/.test(m.split(":")[1] ?? "") },
  { kind: "npm_token", re: new RegExp(`${B}npm_[A-Za-z0-9]{36}(?![A-Za-z0-9])`, "g") },
  { kind: "gitlab_token", re: new RegExp(`${B}glpat-[A-Za-z0-9_-]{20,}`, "g") },
  { kind: "huggingface_token", re: new RegExp(`${B}hf_[A-Za-z]{34}(?![A-Za-z])`, "g") },
  { kind: "groq_key", re: new RegExp(`${B}gsk_[A-Za-z0-9]{40,}`, "g") },
  { kind: "digitalocean_token", re: new RegExp(`${B}dop_v1_[0-9a-f]{64}(?![0-9a-f])`, "g") }
];

// A private key block runs to its END line, or to the end of the text when it is cut off.
const PRIVATE_KEY = /-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)/g;

/**
 * A text is scanned in windows of this many UTF-16 units, so one huge paste costs a bounded amount of
 * work per window and never a quadratic one. Mirrors `SCAN_WINDOW` in `secret_store::detect`.
 */
const SCAN_WINDOW = 64 * 1024;
/** Each window also reads this much on either side, so a value or its label across an edge is seen whole. */
const SCAN_CONTEXT = 4 * 1024;

/** The nearest index at or before `i` (clamped to the text) that does not split a surrogate pair. */
function floorBoundary(text: string, i: number): number {
  const at = Math.min(Math.max(i, 0), text.length);
  const low = text.charCodeAt(at);
  const high = text.charCodeAt(at - 1);
  return at > 0 && at < text.length && low >= 0xdc00 && low <= 0xdfff && high >= 0xd800 && high <= 0xdbff ? at - 1 : at;
}

/**
 * Candidates in text order, without overlaps (an earlier, longer match wins; a shape beats a label).
 * A candidate belongs to the window that holds its first unit; a shape longer than `SCAN_CONTEXT` that
 * crosses a window edge is cut at the window's far edge (no real credential is that long).
 */
export function detectSecrets(text: string): SecretCandidate[] {
  // A private key block can be long, so it is found over the whole text.
  const known: SecretCandidate[] = [];
  PRIVATE_KEY.lastIndex = 0;
  for (const m of text.matchAll(PRIVATE_KEY)) {
    known.push({ kind: "private_key", start: m.index ?? 0, end: (m.index ?? 0) + m[0].length });
  }
  const labelled: SecretCandidate[] = [];
  for (let ownStart = 0; ; ) {
    const ownEnd = floorBoundary(text, ownStart + SCAN_WINDOW);
    const from = floorBoundary(text, ownStart - SCAN_CONTEXT);
    const to = floorBoundary(text, ownEnd + SCAN_CONTEXT);
    const window = from === 0 && to === text.length ? text : text.slice(from, to);
    const owned = (c: SecretCandidate): SecretCandidate[] => {
      const start = c.start + from;
      return start >= ownStart && start < ownEnd ? [{ kind: c.kind, start, end: c.end + from }] : [];
    };
    known.push(...[...detectShapes(window), ...detectCards(window)].flatMap(owned));
    labelled.push(...detectLabelled(window).flatMap(owned));
    if (ownEnd >= text.length) break;
    ownStart = ownEnd;
  }
  const found = [...dropOverlapping(known, labelled), ...known];
  found.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: SecretCandidate[] = [];
  for (const c of found) {
    if (out.length === 0 || c.start >= out[out.length - 1].end) out.push(c);
  }
  return out;
}

function detectShapes(text: string): SecretCandidate[] {
  const out: SecretCandidate[] = [];
  for (const { kind, re, ok } of SHAPES) {
    re.lastIndex = 0;
    for (const m of text.matchAll(re)) {
      if (ok && !ok(m[0])) continue;
      out.push({ kind, start: m.index ?? 0, end: (m.index ?? 0) + m[0].length });
    }
  }
  return out;
}

/** `later` without the candidates that share a unit with one of `first`, in `O((n + m) log n)`. */
function dropOverlapping(first: readonly SecretCandidate[], later: readonly SecretCandidate[]): SecretCandidate[] {
  const sorted = first.map((c) => [c.start, c.end] as const).sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  // reach[i] is the furthest end among the first i + 1 spans.
  const reach: number[] = [];
  for (const [, end] of sorted) reach.push(Math.max(end, reach.length > 0 ? reach[reach.length - 1] : 0));
  return later.filter((c) => {
    // Spans that start before `c` ends; one of them must reach past `c.start`.
    let lo = 0;
    let hi = sorted.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (sorted[mid][0] < c.end) lo = mid + 1;
      else hi = mid;
    }
    return lo === 0 || reach[lo - 1] <= c.start;
  });
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
/**
 * The kinds the composer must warn about before this send. `allowOnce` is the owner's "Send anyway"
 * for exactly this call: it is a plain argument (never stored), so the next send checks again.
 */
export function kindsToWarn(text: string, allowOnce: boolean): string[] {
  return allowOnce ? [] : detectSecrets(text).map((c) => c.kind);
}

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

// ----------------------------------------------------------------- cards ---

const ALNUM = /[\p{Alphabetic}\p{N}]/u;
const isDigit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";

/** Words (folded) that make a contiguous digit run a card number. */
const CARD_LABELS = new Set(["card", "cc", "ccn", "visa", "mastercard", "amex", "kart", "kredi", "banka"]);

/** Digit group sizes people write a card number in. */
const CARD_LAYOUTS = ["4,4,4,4", "4,4,4,4,3", "4,6,5", "4,6,4"];

function luhnOk(d: readonly number[]): boolean {
  let sum = 0;
  for (let i = 0; i < d.length; i++) {
    let x = d[d.length - 1 - i];
    if (i % 2 === 1) {
      x *= 2;
      if (x > 9) x -= 9;
    }
    sum += x;
  }
  return sum % 10 === 0;
}

/** A real card network prefix with the matching length (Visa, Mastercard, Amex, Discover, Diners, JCB, UnionPay, Troy). */
function cardNetworkOk(d: readonly number[]): boolean {
  const n = d.length;
  const p = (k: number) => d.slice(0, k).reduce((a, x) => a * 10 + x, 0);
  const between = (v: number, lo: number, hi: number) => v >= lo && v <= hi;
  switch (d[0]) {
    case 4:
      return n === 13 || n === 16 || n === 19;
    case 5:
      return n === 16 && between(p(2), 51, 55);
    case 2:
      return n === 16 && between(p(4), 2221, 2720);
    case 3:
      switch (p(2)) {
        case 34:
        case 37:
          return n === 15;
        case 36:
        case 38:
        case 39:
          return between(n, 14, 16);
        case 30:
          return n === 14 && between(p(3), 300, 305);
        case 35:
          return between(n, 16, 19) && between(p(4), 3528, 3589);
        default:
          return false;
      }
    case 6:
      return between(n, 16, 19) && (p(4) === 6011 || p(2) === 65 || between(p(3), 644, 649) || p(2) === 62);
    case 9:
      return n === 16 && p(4) === 9792;
    default:
      return false;
  }
}

/** The code point ending just before UTF-16 index `i` (or starting at `i` for `charAt`), as a string. */
function charBefore(text: string, i: number): string {
  return i <= 0 ? "" : (Array.from(text.slice(Math.max(0, i - 2), i)).pop() ?? "");
}
function charAt(text: string, i: number): string {
  return i >= text.length ? "" : (String.fromCodePoint(text.codePointAt(i) ?? 0));
}

/** True when one of the last few words before `start` is a card label. */
function cardLabelBefore(text: string, start: number): boolean {
  // Only the last 32 code points count, and a code point is at most 2 units: never copy the whole prefix.
  const tail = Array.from(text.slice(Math.max(0, start - 64), start)).slice(-32).map(fold).join("");
  const words = tail.split(/[^A-Za-z0-9]+/).filter((w) => w !== "");
  return words.slice(-4).some((w) => CARD_LABELS.has(w) || w.startsWith("kart") || w.startsWith("kredi"));
}

/** A digit run glued to a letter, `+`, `#` or a decimal point is a phone number, an id or an amount. */
function prevBlocksCard(text: string, start: number): boolean {
  const c = charBefore(text, start);
  if (c !== "" && (ALNUM.test(c) || c === "+" || c === "#" || c === "_")) return true;
  return (c === "." || c === ",") && isDigit(text[start - 2]);
}
function nextBlocksCard(text: string, end: number): boolean {
  const c = charAt(text, end);
  if (c !== "" && (ALNUM.test(c) || c === "_")) return true;
  return (c === "." || c === ",") && isDigit(text[end + 1]);
}

function detectCards(text: string): SecretCandidate[] {
  const out: SecretCandidate[] = [];
  let i = 0;
  while (i < text.length) {
    if (!isDigit(text[i])) {
      i++;
      continue;
    }
    // A maximal chain of digit groups joined by one space or dash.
    const start = i;
    const groups: number[] = [];
    let sep: string | null = null;
    let mixed = false;
    for (;;) {
      const g0 = i;
      while (isDigit(text[i])) i++;
      groups.push(i - g0);
      if ((text[i] === " " || text[i] === "-") && isDigit(text[i + 1])) {
        if (sep !== null && sep !== text[i]) mixed = true;
        sep = text[i];
        i++;
      } else {
        break;
      }
    }
    const end = i;
    if (mixed || prevBlocksCard(text, start) || nextBlocksCard(text, end)) continue;
    const digits = [...text.slice(start, end)].filter(isDigit).map(Number);
    if (digits.length < 13 || digits.length > 19) continue;
    const layoutOk = groups.length === 1 ? cardLabelBefore(text, start) : CARD_LAYOUTS.includes(groups.join(","));
    const same = digits.every((d) => d === digits[0]);
    if (layoutOk && !same && cardNetworkOk(digits) && luhnOk(digits)) {
      out.push({ kind: "payment_card", start, end });
    }
  }
  return out;
}

// ---------------------------------------------------------------- labels ---

type LabelClass = { machine: string } | "password";

/** One char folded to lowercase ASCII where Turkish letters have an ASCII twin (`ŞİFRE` = `sifre`). */
function fold(c: string): string {
  switch (c) {
    case "ş": case "Ş": return "s";
    case "ğ": case "Ğ": return "g";
    case "ü": case "Ü": return "u";
    case "ö": case "Ö": return "o";
    case "ç": case "Ç": return "c";
    case "ı": case "İ": case "I": return "i";
    default: return c >= "A" && c <= "Z" ? c.toLowerCase() : c;
  }
}

/** Classify a folded word as a label (full-word forms plus the Turkish possessive / case suffixes of the stem). */
function labelWord(word: string): LabelClass | null {
  const stem = (s: string) => word.startsWith(s) && word.length - s.length <= 4 && /^[a-z]*$/.test(word.slice(s.length));
  switch (word) {
    case "token": case "bearer": case "jeton": return { machine: "labelled_token" };
    case "secret": return { machine: "labelled_secret" };
    case "apikey": return { machine: "labelled_api_key" };
    case "password": case "passwd": case "pwd": case "passphrase": return "password";
  }
  if (stem("sifre") || stem("parola")) return "password";
  if (stem("anahtar")) return { machine: "labelled_api_key" };
  if (stem("jeton")) return { machine: "labelled_token" };
  return null;
}

/** Words that may sit between a label and its value ("password is X"). */
const GLUE = new Set(["is", "was", "are", "ise", "su", "budur", "olarak", "sunu", "key", "anahtar"]);

/** Words that, before `key` / `anahtar`, make a two-word label. */
function labelPrefix(word: string): LabelClass | null {
  switch (word) {
    case "api": case "access": case "erisim": return { machine: "labelled_api_key" };
    case "gizli": case "secret": case "private": case "client": return { machine: "labelled_secret" };
    default: return null;
  }
}

interface Word {
  /** Index of the first / one past the last char in the code-point array. */
  first: number;
  last: number;
  folded: string;
}

const MAX_GAP_CHARS = 24;
/** Chars of a labelled value that are read before giving up on it: the longest value (512) plus trailing punctuation. */
const MAX_VALUE_SCAN = 520;
const WHITESPACE = /\s/u;

function detectLabelled(text: string): SecretCandidate[] {
  // Code points with their UTF-16 offset (plus the end), so spans index the original string.
  const chars = Array.from(text);
  const offsets: number[] = [];
  let at = 0;
  for (const c of chars) {
    offsets.push(at);
    at += c.length;
  }
  offsets.push(at);
  const folded = chars.map(fold);

  // Words: runs of alphanumerics (`_`, `-` and `.` separate words, so `OPENAI_API_KEY` is `openai`, `api`, `key`).
  const words: Word[] = [];
  for (let i = 0; i < chars.length; ) {
    if (ALNUM.test(chars[i])) {
      const first = i;
      while (i < chars.length && ALNUM.test(chars[i])) i++;
      words.push({ first, last: i, folded: folded.slice(first, i).join("") });
    } else {
      i++;
    }
  }

  const out: SecretCandidate[] = [];
  let wi = 0;
  while (wi < words.length) {
    const w = words[wi];
    let cls = labelWord(w.folded);
    let labelLast = w.last;
    let consumed = 1;
    // Two-word labels: "api key", "secret key", "gizli anahtar", ...
    const prefix = labelPrefix(w.folded);
    const next = words[wi + 1];
    if (prefix && next) {
      const contiguous = chars.slice(w.last, next.first).every((c) => c === " " || c === "_" || c === "-");
      const keyWord = next.folded === "key" || next.folded === "keys" || next.folded.startsWith("anahtar");
      if (contiguous && keyWord) {
        cls = prefix;
        labelLast = next.last;
        consumed = 2;
      } else if (contiguous && labelWord(next.folded)) {
        // "api token", "client secret": the second word decides.
        wi++;
        continue;
      }
    }
    // A placeholder `[secret: name]` is our own output, never a candidate.
    if (!cls || (w.first > 0 && chars[w.first - 1] === "[")) {
      wi += consumed;
      continue;
    }
    const c = valueAfterLabel(chars, offsets, words, wi + consumed, labelLast, cls);
    if (c) out.push(c);
    wi += consumed;
  }
  return out;
}

const VALUE_END = new Set([",", ";", "<", ">", ")", "]", "}", "«", "»", "“", "”", "‘", "’", '"', "'", "`"]);

/** Parse the value following a label that ends at code-point index `labelLast`. */
function valueAfterLabel(
  chars: readonly string[],
  offsets: readonly number[],
  words: readonly Word[],
  nextWord: number,
  labelLast: number,
  cls: LabelClass
): SecretCandidate | null {
  const isGap = (c: string) => c === " " || c === "\t" || c === ":" || c === "=" || c === '"' || c === "'" || c === "`" || c === ">";
  let p = labelLast;
  let nw = nextWord;
  const gapStart = p;
  for (;;) {
    while (p < chars.length && p - gapStart <= MAX_GAP_CHARS && isGap(chars[p])) p++;
    // Skip one glue word ("is", "olarak", "şu"...), then more gap chars.
    const w = words[nw];
    if (w && w.first === p && GLUE.has(w.folded)) {
      p = w.last;
      nw++;
      continue;
    }
    break;
  }
  if (p === gapStart || p - gapStart > MAX_GAP_CHARS || p >= chars.length) return null;
  const startChar = p;
  // Look at no more than MAX_VALUE_SCAN chars: a longer run is no value (the checks below allow 512 at most),
  // and `token:token:token:...` must not make every label rescan the rest of the text.
  const scanTo = Math.min(chars.length, startChar + MAX_VALUE_SCAN);
  while (p < scanTo && !WHITESPACE.test(chars[p]) && !VALUE_END.has(chars[p])) p++;
  if (p === scanTo && scanTo < chars.length) return null;
  // Trailing sentence punctuation is not part of the value.
  while (p > startChar && (chars[p - 1] === "." || chars[p - 1] === ":" || chars[p - 1] === "?")) p--;
  if (p <= startChar) return null;
  const value = chars.slice(startChar, p).join("");
  const ok = cls === "password" ? looksLikePassword(value) : looksLikeMachineValue(value);
  if (!ok) return null;
  return {
    kind: cls === "password" ? "labelled_password" : cls.machine,
    start: offsets[startChar],
    end: offsets[p]
  };
}

/** Shannon entropy in bits per char. */
function entropy(value: string): number {
  const counts = new Map<string, number>();
  let n = 0;
  for (const c of value) {
    counts.set(c, (counts.get(c) ?? 0) + 1);
    n++;
  }
  if (n === 0) return 0;
  let h = 0;
  for (const k of counts.values()) h -= (k / n) * Math.log2(k / n);
  return h;
}

const count = (value: string, ch: string) => value.split(ch).length - 1;

function looksLikeUrlOrPath(value: string): boolean {
  return (
    value.includes("://") ||
    /^[/~\\.]/.test(value) ||
    count(value, "/") >= 2 ||
    count(value, "\\") >= 1 ||
    value.slice(1, 3) === ":\\" ||
    value.slice(1, 3) === ":/"
  );
}

/** Number of letter/digit class changes along the ASCII alphanumerics of `value`. */
function classChanges(value: string): number {
  let last: boolean | null = null;
  let n = 0;
  for (const c of value) {
    if (!/[A-Za-z0-9]/.test(c)) continue;
    const digit = isDigit(c);
    if (last !== null && last !== digit) n++;
    last = digit;
  }
  return n;
}

function looksLikeMachineValue(value: string): boolean {
  const len = [...value].length;
  if (len < 16 || len > 512) return false;
  if (!/^[A-Za-z0-9_.+/=~-]*$/.test(value)) return false;
  if (looksLikeUrlOrPath(value)) return false;
  const hasDigit = /[0-9]/.test(value);
  const hasLower = /[a-z]/.test(value);
  const hasUpper = /[A-Z]/.test(value);
  if (!hasDigit || !(hasLower || hasUpper)) return false;
  // A version string ("1.2.3-beta4") is dots and digits around a few words.
  if (value.split(/[.-]/).every((s) => s.length <= 8) && count(value, ".") >= 2) return false;
  if (new Set(value).size < 8 || entropy(value) < 3.0) return false;
  // Words with a number glued on ("my-long-token-name-2024") are text, not keys.
  return (hasLower && hasUpper) || classChanges(value) >= 3;
}

function looksLikePassword(value: string): boolean {
  const chars = [...value];
  if (chars.length < 8 || chars.length > 128) return false;
  if ("<[{$%*(".includes(value[0]) || value.includes("://")) return false;
  if (looksLikeUrlOrPath(value) && !/[0-9]/.test(value)) return false;
  const hasLower = /\p{Lowercase}/u.test(value);
  const hasUpper = /\p{Uppercase}/u.test(value);
  const hasDigit = /[0-9]/.test(value);
  const hasSymbol = chars.some((c) => !ALNUM.test(c));
  // `.`, `_` and `-` alone do not make a password (`getpass.getpass`).
  const hasStrongSymbol = chars.some((c) => !ALNUM.test(c) && c !== "." && c !== "_" && c !== "-");
  const classes = [hasLower, hasUpper, hasDigit, hasSymbol].filter(Boolean).length;
  return (
    (hasLower || hasUpper) &&
    (hasDigit || hasStrongSymbol) &&
    !value.includes("(") &&
    classes >= 2 &&
    new Set(chars).size >= 5
  );
}
