/** Feature copy that ships in English and Turkish and uses English elsewhere. */
export function usesEnglishFallback(locale: string, key: string): boolean {
  if (locale !== "de" && locale !== "es") return false;
  return (
    key.startsWith("apps.composioHub.") ||
    // The newer pasted-secret kinds (labelled values, card numbers), #421.
    key.startsWith("pastedSecret.kind.labelled_") ||
    key === "pastedSecret.kind.payment_card"
  );
}
