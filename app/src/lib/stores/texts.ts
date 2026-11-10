// Texts: the one being written (the send dialog) and those received, oldest first. Received texts live only here,
// in memory: they may be passwords, so they are never stored.

import { writable } from 'svelte/store';

import { notify, onEvent, sendText } from '$lib/api';

import type { Peer, ReceivedText } from '$lib/types';

/** Received texts not closed yet. The first one is shown in a dialog. */
export const receivedTexts = writable<ReceivedText[]>([]);

/** The device the send dialog is open for, if any. */
export const textTarget = writable<Peer | null>(null);

/** Follows received texts. `notification` builds the translated notification, shown even with the window hidden. */
export async function startTexts(notification: (text: ReceivedText) => { title: string; body: string }): Promise<void> {
  await onEvent('text-received', (text) => {
    receivedTexts.update((list) => [...list, text]);
    const { title, body } = notification(text);
    void notify(title, body);
  });
}

/** Closes the oldest received text. */
export function closeReceivedText(): void {
  receivedTexts.update((list) => list.slice(1));
}

export function openSendText(peer: Peer): void {
  textTarget.set(peer);
}

export function closeSendText(): void {
  textTarget.set(null);
}

/** Sends `text` to `peer`; resolves once the other device has it. */
export async function sendTextTo(peer: Peer, text: string): Promise<void> {
  await sendText(peer.id, text);
}

/** Bytes of `text` in UTF-8, the unit the size limit is counted in. */
export function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}
