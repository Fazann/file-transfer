// Finished transfers from the history database.

import { writable } from 'svelte/store';

import { clearHistory as clearAll, getHistory, onEvent } from '$lib/api';

import type { HistoryEntry } from '$lib/types';

export const history = writable<HistoryEntry[]>([]);

export async function loadHistory(): Promise<void> {
  history.set(await getHistory());
}

/** Loads the history and reloads it whenever a transfer ends. */
export async function startHistory(): Promise<void> {
  // The Rust side writes the entry right after emitting the event, so wait a moment before reading.
  const reload = (): void => void setTimeout(() => void loadHistory(), 300);
  await Promise.all([onEvent('transfer-complete', reload), onEvent('transfer-failed', reload)]);
  await loadHistory();
}

export async function clearHistory(): Promise<void> {
  await clearAll();
  history.set([]);
}
