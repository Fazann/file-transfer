// The update offered to the user, if any.

import { writable } from 'svelte/store';

import { checkForUpdate, type AvailableUpdate } from '$lib/api';

export const update = writable<AvailableUpdate | null>(null);

/** Looks for an update once. LanShare works without internet access, so a failed check is ignored. */
export async function startUpdateCheck(): Promise<void> {
  if (!import.meta.env.PROD) {
    return;
  }
  try {
    update.set(await checkForUpdate());
  } catch {
    update.set(null);
  }
}

export function dismissUpdate(): void {
  update.set(null);
}
