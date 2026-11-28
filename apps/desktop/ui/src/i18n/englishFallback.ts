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
    key === "pastedSecret.kind.payment_card" ||
    // The model picker's short "set up" row for a coding CLI that is not ready on the server.
    key === "modelPicker.missingCliShort" ||
    key === "modelPicker.setUpCli" ||
    // Standing orders (quiet checks), English and Turkish first.
    key === "schedules.form.payloadKeptStanding" ||
    key === "schedules.payload.standingOrder" ||
    key.startsWith("schedules.standing.") ||
    key.startsWith("schedules.form.taskKind.") ||
    key.startsWith("schedules.form.standing.") ||
    key === "schedules.form.invalid.checklist" ||
    key === "schedules.form.invalid.maxRuns" ||
    key === "schedules.form.invalid.hoursStart" ||
    key === "schedules.form.invalid.hoursEnd" ||
    key === "schedules.form.invalid.hoursTz"
  );
}
