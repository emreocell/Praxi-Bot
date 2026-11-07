/**
 * Fuzzy matching for the command palette and the composer menus.
 *
 * A query matches when its characters appear in order in the text
 * (case-, diacritic- and Turkish-dotless-i-insensitive). Better matches score
 * higher: a prefix, word starts and consecutive runs beat scattered hits.
 */

/** Fold one UTF-16 unit; the result is always exactly one unit long. */
function foldChar(ch: string): string {
  if (ch === "ı" || ch === "İ") {
    return "i";
  }
  const base = ch.normalize("NFD").charAt(0) || ch;
  return base.toLowerCase().charAt(0) || ch;
}

/** Length-preserving fold, so match indices point into the original string. */
export function fold(s: string): string {
  let out = "";
  for (const ch of s) {
    // iterate by code point, fold each unit of it
    out += ch.length === 1 ? foldChar(ch) : ch.toLowerCase().slice(0, ch.length);
  }
  return out;
}

export interface FuzzyMatch {
  score: number;
  /** Indices (into the original text) of the matched characters. */
  indices: number[];
}

function isBoundary(text: string, i: number): boolean {
  if (i === 0) {
    return true;
  }
  const prev = text.charAt(i - 1);
  return /[\s\-_./:@#>]/.test(prev);
}

/** `null` when `query` does not match `text`. An empty query matches with 0. */
export function fuzzyMatch(query: string, text: string): FuzzyMatch | null {
  const q = fold(query.trim());
  if (q === "") {
    return { score: 0, indices: [] };
  }
  const f = fold(text);
  if (q.length > f.length) {
    return null;
  }

  // Greedy forward pass finds the end; a backward pass then tightens the span,
  // which prefers compact matches over ones stretched across the whole text.
  let ti = 0;
  let qi = 0;
  while (ti < f.length && qi < q.length) {
    if (f[ti] === q[qi]) {
      qi++;
    }
    ti++;
  }
  if (qi < q.length) {
    return null;
  }
  let end = ti - 1;
  let qj = q.length - 1;
  const indices: number[] = new Array<number>(q.length);
  for (let k = end; k >= 0 && qj >= 0; k--) {
    if (f[k] === q[qj]) {
      indices[qj] = k;
      qj--;
    }
  }
  end = indices[indices.length - 1] ?? end;

  let score = 0;
  const substring = f.indexOf(q);
  if (substring === 0) {
    score += 60;
  } else if (substring > 0) {
    score += isBoundary(text, substring) ? 40 : 25;
  }
  for (let k = 0; k < indices.length; k++) {
    const idx = indices[k] ?? 0;
    score += 10;
    if (isBoundary(text, idx)) {
      score += 8;
    }
    if (k > 0 && idx === (indices[k - 1] ?? -2) + 1) {
      score += 12;
    }
  }
  // shorter texts and earlier matches rank first among equals
  score -= (indices[0] ?? 0) * 0.5;
  score -= (end - (indices[0] ?? 0) + 1 - q.length) * 1.5;
  score -= f.length * 0.05;
  return { score, indices };
}

export interface Ranked<T> {
  item: T;
  score: number;
  /** Matched indices into the field that matched best (`text`). */
  indices: number[];
}

/**
 * Rank `items` by `query`. Each item offers a primary text plus optional
 * keywords (matched with a penalty and without highlight). Empty query keeps
 * the original order. Ties keep the original order.
 */
export function rank<T>(
  items: readonly T[],
  query: string,
  fields: (item: T) => { text: string; keywords?: readonly string[] }
): Ranked<T>[] {
  if (query.trim() === "") {
    return items.map((item) => ({ item, score: 0, indices: [] }));
  }
  const out: (Ranked<T> & { order: number })[] = [];
  items.forEach((item, order) => {
    const { text, keywords } = fields(item);
    const main = fuzzyMatch(query, text);
    let best: Ranked<T> | null = main
      ? { item, score: main.score, indices: main.indices }
      : null;
    for (const kw of keywords ?? []) {
      const m = fuzzyMatch(query, kw);
      if (m && (!best || m.score - 15 > best.score)) {
        best = { item, score: m.score - 15, indices: [] };
      }
    }
    if (best) {
      out.push({ ...best, order });
    }
  });
  return out
    .sort((a, b) => b.score - a.score || a.order - b.order)
    .map(({ item, score, indices }) => ({ item, score, indices }));
}
