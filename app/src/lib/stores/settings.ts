// App settings, loaded once and saved through the Settings page.

import { writable } from 'svelte/store';

import { getSettings, updateSettings } from '$lib/api';

import type { Settings } from '$lib/types';

export const settings = writable<Settings | null>(null);

export async function loadSettings(): Promise<Settings> {
  const loaded = await getSettings();
  settings.set(loaded);
  return loaded;
}

/** Saves and keeps what the Rust side stored (it corrects values out of range). */
export async function saveSettings(next: Settings): Promise<Settings> {
  const saved = await updateSettings(next);
  settings.set(saved);
  return saved;
}
