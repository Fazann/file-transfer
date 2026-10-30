// svelte-i18n setup: English (source and fallback), Khmer and Arabic, bundled with the app.

import { addMessages, init } from 'svelte-i18n';

import ar from './locales/ar.json';
import en from './locales/en.json';
import km from './locales/km.json';

import type { Locale } from '$lib/types';

/** Every supported language, in the order the language picker shows them. */
export const LOCALES: readonly Locale[] = ['en', 'km', 'ar'];

addMessages('en', en);
addMessages('km', km);
addMessages('ar', ar);

/** Whether `value` is a supported language. */
export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value);
}

/** The operating system's language if it is supported, else English. */
export function systemLocale(): Locale {
  const languages = typeof navigator === 'undefined' ? [] : navigator.languages;
  for (const tag of languages) {
    const language = tag.split('-')[0];
    if (isLocale(language)) {
      return language;
    }
  }
  return 'en';
}

/** Starts i18n in the saved language, else the system one. */
export function setupI18n(saved: Locale | null): Locale {
  const initial = saved ?? systemLocale();
  init({ fallbackLocale: 'en', initialLocale: initial });
  return initial;
}

/** Sets `<html lang dir>`: right-to-left for Arabic, left-to-right otherwise. */
export function applyDocumentLanguage(locale: Locale): void {
  document.documentElement.lang = locale;
  document.documentElement.dir = locale === 'ar' ? 'rtl' : 'ltr';
}
