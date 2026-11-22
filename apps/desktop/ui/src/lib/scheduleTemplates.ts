import type { Frequency } from "./scheduleBuilder.ts";

export interface TemplateText {
  name: string;
  blurb: string;
  prompt: string;
}

/** One ready-made job (`data/scheduleTemplates.json`): plain text in every language. */
export interface ScheduleTemplate {
  id: string;
  /** lucide icon name, resolved by the view. */
  icon: string;
  freq: Frequency;
  time: string;
  weekday?: number;
  text: Record<string, TemplateText>;
}

export const TEMPLATE_LANGS = ["en", "tr", "de"] as const;

/** The text in `lang`, falling back to English (a missing language never hides a template). */
export function templateText(t: ScheduleTemplate, lang: string): TemplateText {
  const short = lang.toLowerCase().split("-")[0] ?? "en";
  return t.text[short] ?? t.text.en!;
}
