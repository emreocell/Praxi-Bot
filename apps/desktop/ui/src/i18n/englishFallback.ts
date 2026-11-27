/** Feature copy that ships in English and Turkish and uses English elsewhere. */
export function usesEnglishFallback(locale: string, key: string): boolean {
  return (locale === "de" || locale === "es") && key.startsWith("apps.composioHub.");
}
