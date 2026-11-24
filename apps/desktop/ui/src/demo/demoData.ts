import de from "./demo.de.json";
import en from "./demo.en.json";
import tr from "./demo.tr.json";
import type { DemoData } from "./demoEngine.ts";

const BY_LANGUAGE: Record<string, DemoData> = { en, tr, de } as Record<string, DemoData>;

/** The demo texts in the user's language, English when that language has none. */
export function demoData(language: string): DemoData {
  return BY_LANGUAGE[language.slice(0, 2).toLowerCase()] ?? BY_LANGUAGE.en;
}
