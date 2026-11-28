/** The extra "When something happens" sources (feed, web page, calendar, Home Assistant). */
const MORE_EVENT_KEYS = /^(?:automation\.(?:builder\.title|events|what)\.(?:feed|page|calendar|home_assistant)(?:\.|$)|botPanel\.triggers\.(?:when_(?:feed|page|calendar|home_assistant)|url|selector|selectorHint|feedHint|pageHint|pollMinutes|calendar|anyCalendar|minutesBefore|calendarHint|entity|toState|toStateHint|haHint)$)/;

/** Feature copy that ships in English and Turkish and uses English elsewhere (Composio hub, REST APIs). */
export function usesEnglishFallback(locale: string, key: string): boolean {
  if (locale !== "de" && locale !== "es") return false;
  return (
    MORE_EVENT_KEYS.test(key) ||
    key.startsWith("apps.composioHub.") ||
    key.startsWith("apps.restApis.") ||
    key.startsWith("chat.step.desktop_clipboard.") ||
    key.startsWith("chat.step.desktop_element.") ||
    key.startsWith("chat.step.desktop_window.") ||
    // The newer pasted-secret kinds (labelled values, card numbers), #421.
    key.startsWith("pastedSecret.kind.labelled_") ||
    key === "pastedSecret.kind.payment_card"
  );
}
