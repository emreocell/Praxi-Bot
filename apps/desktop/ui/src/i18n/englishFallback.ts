/** Feature copy that ships in English and Turkish and uses English elsewhere (Composio hub, REST APIs). */
export function usesEnglishFallback(locale: string, key: string): boolean {
  if (locale !== "de" && locale !== "es") return false;
  return (
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
    key === "modelPicker.setUpCli"
  );
}
