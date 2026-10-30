// Incoming offers waiting for the user, oldest first. The first one is shown in a dialog.

import { writable } from 'svelte/store';

import { notify, onEvent, respondToOffer } from '$lib/api';

import type { IncomingOffer } from '$lib/types';

export const offers = writable<IncomingOffer[]>([]);

function remove(requestId: number): void {
  offers.update((list) => list.filter((offer) => offer.requestId !== requestId));
}

/**
 * Follows offers. `notification` builds the translated notification text, shown even while the window is hidden
 * in the tray.
 */
export async function startOffers(
  notification: (offer: IncomingOffer) => { title: string; body: string },
): Promise<void> {
  await Promise.all([
    onEvent('incoming-offer', (offer) => {
      offers.update((list) => [...list, offer]);
      const { title, body } = notification(offer);
      void notify(title, body);
    }),
    onEvent('offer-closed', remove),
  ]);
}

export async function answerOffer(requestId: number, accept: boolean): Promise<void> {
  remove(requestId);
  await respondToOffer(requestId, accept);
}
