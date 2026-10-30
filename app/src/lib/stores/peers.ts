// Devices on the network and this device, kept current by events.

import { writable } from 'svelte/store';

import { getSelf, listPeers, onEvent } from '$lib/api';

import type { Peer, SelfInfo } from '$lib/types';

export const peers = writable<Peer[]>([]);
export const self = writable<SelfInfo | null>(null);

function byName(left: Peer, right: Peer): number {
  return left.name.localeCompare(right.name);
}

function upsert(peer: Peer): void {
  peers.update((list) => [...list.filter((known) => known.id !== peer.id), peer].sort(byName));
}

/** Marks a device as paired right away, before discovery reports it again. */
export function markTrusted(peerId: string, trusted: boolean): void {
  peers.update((list) => list.map((peer) => (peer.id === peerId ? { ...peer, trusted } : peer)));
}

/** Loads this device and the current devices, and follows changes. */
export async function startPeers(): Promise<void> {
  await Promise.all([
    onEvent('peer-found', upsert),
    onEvent('peer-updated', upsert),
    onEvent('peer-lost', (id) => peers.update((list) => list.filter((peer) => peer.id !== id))),
  ]);
  const [me, found] = await Promise.all([getSelf(), listPeers()]);
  self.set(me);
  peers.set([...found].sort(byName));
}

/** Reloads this device's info, for example after its name changed in Settings. */
export async function refreshSelf(): Promise<void> {
  self.set(await getSelf());
}
