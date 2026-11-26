import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import tr from "./locales/tr.json";
import de from "./locales/de.json";
import es from "./locales/es.json";
import fr from "./locales/fr.json";
import pt from "./locales/pt.json";
import id from "./locales/id.json";

export type SupportedLanguage = "en" | "tr" | "de" | "es" | "fr" | "pt" | "id";

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    tr: { translation: tr },
    de: { translation: de },
    es: { translation: es },
    fr: { translation: fr },
    pt: { translation: pt },
    id: { translation: id }
  },
  lng: "en",
  fallbackLng: "en",
  interpolation: {
    escapeValue: false
  }
});

// The page language follows the app language, so `text-transform: uppercase` uses the right rules
// ("bilgisayarlar" is "BİLGİSAYARLAR" in Turkish, not "BILGISAYARLAR") and screen readers pick the voice.
i18n.on("languageChanged", (language: string) => {
  if (typeof document !== "undefined") document.documentElement.lang = language;
});

export { i18n };
