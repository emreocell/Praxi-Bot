// Pure helpers for the language switcher (kept apart from the Vue file so Node can test them).
export type Lang = "en" | "tr" | "de";
export const LANGS: { code: Lang; label: string }[] = [
  { code: "en", label: "English" },
  { code: "tr", label: "Türkçe" },
  { code: "de", label: "Deutsch" },
];

export function currentLang(relativePath: string): Lang {
  const m = /^guide\/(tr|de)\//.exec(relativePath);
  return (m ? m[1] : "en") as Lang;
}

/** Page path (no base, no extension) of the same page in `target`, or the best fallback. */
export function switchTarget(relativePath: string, target: Lang, translated: Record<string, string[]>): string {
  const page = relativePath.replace(/\.md$/, "");
  const guide = /^guide\/(?:(?:tr|de)\/)?(.*)$/.exec(page);
  if (!guide) {
    // Only the guide is translated; other pages stay English, so tr/de land on their guide index.
    return target === "en" ? page : `guide/${target}/index`;
  }
  const name = guide[1] || "index";
  if (target === "en") return `guide/${name}`;
  if (name === "index" || translated[target]?.includes(name)) return `guide/${target}/${name}`;
  // Not translated yet: the page opens in English, as docs/contributing/translating-the-guide.md promises.
  return `guide/${name}`;
}
