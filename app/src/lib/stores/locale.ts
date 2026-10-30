// The UI language. Changing it applies at once: messages, `<html lang dir>`, and (from App) the tray labels.

import { writable } from 'svelte/store';
import { locale as i18nLocale } from 'svelte-i18n';

import { applyDocumentLanguage } from '$lib/i18n';

import type { Locale } from '$lib/types';

export const locale = writable<Locale>('en');

export function setLocale(next: Locale): void {
  i18nLocale.set(next);
  applyDocumentLanguage(next);
  locale.set(next);
}
