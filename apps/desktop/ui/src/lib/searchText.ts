/** Lower-case, strip diacritics and fold the Turkish dotless i, for forgiving search. */
export function foldText(s: string): string {
  return s
    .replace(/İ/g, "i")
    .replace(/ı/g, "i")
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase();
}

/** True when every word of `query` occurs in one of `texts` (empty query matches). */
export function matchesQuery(
  query: string,
  ...texts: Array<string | null | undefined>
): boolean {
  const words = foldText(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) {
    return true;
  }
  const hay = foldText(texts.filter(Boolean).join(" "));
  return words.every((w) => hay.includes(w));
}
