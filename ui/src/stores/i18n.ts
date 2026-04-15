import { createSignal } from "solid-js";
import en from "../locales/en.json";
import es from "../locales/es.json";
import de from "../locales/de.json";
import fr from "../locales/fr.json";
import zh from "../locales/zh.json";
import ar from "../locales/ar.json";
import hi from "../locales/hi.json";

type LocaleStrings = Record<string, string>;

const locales: Record<string, LocaleStrings> = { en, es, de, fr, zh, ar, hi };

export const SUPPORTED_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "de", label: "Deutsch" },
  { code: "fr", label: "Français" },
  { code: "zh", label: "中文" },
  { code: "ar", label: "العربية" },
  { code: "hi", label: "हिन्दी" },
];

const [currentLocale, setCurrentLocale] = createSignal<string>("en");

/**
 * Get a translated string by key.
 * Falls back to English if the key is not found in the current locale.
 */
export function t(key: string): string {
  const lang = currentLocale();
  const strings = locales[lang] || locales["en"];
  return strings[key] || locales["en"][key] || key;
}

/**
 * Change the current locale.
 */
export function setLocale(locale: string) {
  if (locales[locale]) {
    setCurrentLocale(locale);
    // Set dir attribute for RTL languages
    document.documentElement.dir = locale === "ar" ? "rtl" : "ltr";
    document.documentElement.lang = locale;
  }
}

/**
 * Get the current locale code.
 */
export function getLocale(): string {
  return currentLocale();
}

export { currentLocale };
