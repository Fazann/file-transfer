// Short messages shown in a corner and dismissed after a few seconds.

import { writable } from 'svelte/store';

export type ToastTone = 'info' | 'success' | 'error';

export interface ToastMessage {
  id: number;
  text: string;
  tone: ToastTone;
}

const SHOW_FOR_MS = 6000;

let nextId = 1;

export const toasts = writable<ToastMessage[]>([]);

export function dismissToast(id: number): void {
  toasts.update((list) => list.filter((toast) => toast.id !== id));
}

/** Shows `text` (already translated). */
export function showToast(text: string, tone: ToastTone = 'info'): void {
  const id = nextId++;
  toasts.update((list) => [...list, { id, text, tone }]);
  setTimeout(() => dismissToast(id), SHOW_FOR_MS);
}
