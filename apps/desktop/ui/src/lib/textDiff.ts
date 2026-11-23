export type DiffKind = "same" | "added" | "removed";
export type DiffPart = { text: string; kind: DiffKind };

/** Longest text (in words) compared; the daemon already bounds descriptions. */
const MAX_WORDS = 600;

function words(text: string): string[] {
  return text.match(/\s+|\S+/g) ?? [];
}

/**
 * Word-level diff of two texts for the "what changed" card. Returns the
 * `before` side (same + removed) and the `after` side (same + added). Plain
 * strings only: callers render them as text, never as markup.
 */
export function wordDiff(
  before: string,
  after: string
): { before: DiffPart[]; after: DiffPart[] } {
  const a = words(before).slice(0, MAX_WORDS);
  const b = words(after).slice(0, MAX_WORDS);
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () =>
    new Array<number>(b.length + 1).fill(0)
  );
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] =
        a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const left: DiffPart[] = [];
  const right: DiffPart[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      left.push({ text: a[i], kind: "same" });
      right.push({ text: b[j], kind: "same" });
      i++;
      j++;
    } else if (j >= b.length || (i < a.length && lcs[i + 1][j] >= lcs[i][j + 1])) {
      left.push({ text: a[i], kind: "removed" });
      i++;
    } else {
      right.push({ text: b[j], kind: "added" });
      j++;
    }
  }
  return { before: left, after: right };
}
