import de from "../help/faq.de.json";
import en from "../help/faq.en.json";
import guideDe from "../help/guideIndex.de.json";
import guide from "../help/guideIndex.json";
import guideTr from "../help/guideIndex.tr.json";
import tr from "../help/faq.tr.json";
import { containsFolded, foldVariants, guideForLanguage, queryWords, searchGuide, type GuideEntry } from "./guideIndex.ts";

/** One short answer in the Help window. `doc` is a page of docs/guide for "Read more". */
export interface FaqItem {
  id: string;
  q: string;
  a: string;
  doc: string;
}

const BY_LANGUAGE: Record<string, readonly FaqItem[]> = { en, tr, de };

export const DOCS_BASE_URL = "https://github.com/emreocell/Praxi-Bot/blob/main/docs/guide/";
export const DOCS_INDEX_URL = `${DOCS_BASE_URL}index.md`;

/** The answers in the user's language, English when that language has none. */
export function faqFor(language: string): readonly FaqItem[] {
  return BY_LANGUAGE[language.slice(0, 2).toLowerCase()] ?? en;
}

/**
 * Every word of the query must appear in the question or the answer (case, accents, Turkish `ı`
 * and German `ß` / `ae oe ue` ignored, see `foldVariants`), so "free key" finds the free-key
 * answer and typing never needs exact wording.
 */
export function searchFaq(items: readonly FaqItem[], query: string, language: string = ""): FaqItem[] {
  const words = queryWords(query, language);
  if (words.length === 0) {
    return [...items];
  }
  const scored = items.flatMap((item) => {
    const question = foldVariants(item.q, language);
    const text = foldVariants(`${item.q} ${item.a}`, language);
    return words.every((w) => containsFolded(text, w))
      ? [{ item, inQuestion: words.every((w) => containsFolded(question, w)) }]
      : [];
  });
  return scored.sort((a, b) => Number(b.inQuestion) - Number(a.inQuestion)).map((s) => s.item);
}

export function docUrl(item: FaqItem): string {
  return `${DOCS_BASE_URL}${item.doc}`;
}

/** The English guide pages as the Help window searches them. */
export const GUIDE_INDEX: readonly GuideEntry[] = guide;

const GUIDE_BY_LANGUAGE: Record<string, readonly GuideEntry[]> = {
  tr: guideForLanguage(guide, guideTr),
  de: guideForLanguage(guide, guideDe)
};

/** The languages that have their own guide index; every other language searches the English pages. */
export const GUIDE_LANGUAGES: readonly string[] = Object.keys(GUIDE_BY_LANGUAGE);

/** The guide pages in the user's language: translated ones first, English for the rest. */
export function guideFor(language: string): readonly GuideEntry[] {
  return GUIDE_BY_LANGUAGE[language.slice(0, 2).toLowerCase()] ?? GUIDE_INDEX;
}

export { searchGuide };

/** The plain-words pages shown in the Help window before anyone types: read them in this order. */
export const START_PAGES: readonly string[] = [
  "getting-started.md",
  "keep-your-bot-safe.md",
  "local-or-server.md",
  "automate.md",
  "connect-your-apps.md",
  "fix-common-problems.md",
  "quick-faq.md"
];

/** The start pages in the user's language (English for a page that is not translated yet). */
export function startPages(language: string): GuideEntry[] {
  const pages = guideFor(language);
  return START_PAGES.flatMap((name) => pages.filter((p) => p.doc.slice(p.doc.lastIndexOf("/") + 1) === name));
}

export function guideUrl(entry: GuideEntry): string {
  return `${DOCS_BASE_URL}${entry.doc}`;
}
