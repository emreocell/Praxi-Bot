import de from "../help/faq.de.json";
import en from "../help/faq.en.json";
import tr from "../help/faq.tr.json";

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

const fold = (s: string): string => s.toLocaleLowerCase().normalize("NFD").replace(/[\u0300-\u036f]/g, "").replace(/ı/g, "i");

/**
 * Every word of the query must appear in the question or the answer (accents and case ignored),
 * so "free key" finds the free-key answer and typing never needs exact wording.
 */
export function searchFaq(items: readonly FaqItem[], query: string): FaqItem[] {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) {
    return [...items];
  }
  const inQuestion = (item: FaqItem) => words.every((w) => fold(item.q).includes(w));
  return items
    .filter((item) => {
      const text = fold(`${item.q} ${item.a}`);
      return words.every((w) => text.includes(w));
    })
    .sort((a, b) => Number(inQuestion(b)) - Number(inQuestion(a)));
}

export function docUrl(item: FaqItem): string {
  return `${DOCS_BASE_URL}${item.doc}`;
}
