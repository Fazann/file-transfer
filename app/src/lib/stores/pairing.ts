// The pairing dialog: either this device shows a PIN, or the user types the PIN another device shows.

import { get, writable } from 'svelte/store';

import { onEvent, sendFiles, startPairing, submitPin } from '$lib/api';
import { markTrusted } from '$lib/stores/peers';

import type { CommandError, PairingResult, Peer, TrustedDevice } from '$lib/types';

export type PairingDialog =
  | { kind: 'show'; requestId: number; deviceName: string; fingerprint: string; pin: string }
  | { kind: 'enter'; requestId: number; peer: Peer; pendingPaths: string[]; submitted: boolean };

export const pairing = writable<PairingDialog | null>(null);

/** What the UI shows when a pairing ends. */
export interface PairingOutcome {
  device: TrustedDevice | null;
  error: CommandError | null;
  deviceName: string;
}

/** Follows pairing requests from other devices and results. `onDone` shows the outcome. */
export async function startPairingEvents(onDone: (outcome: PairingOutcome) => void): Promise<void> {
  await Promise.all([
    onEvent('pairing-pin', ({ requestId, deviceName, fingerprint, pin }) =>
      pairing.set({ kind: 'show', requestId, deviceName, fingerprint, pin }),
    ),
    onEvent('pairing-result', (result) => void finish(result, onDone)),
  ]);
}

async function finish(result: PairingResult, onDone: (outcome: PairingOutcome) => void): Promise<void> {
  const dialog = get(pairing);
  if (dialog === null || dialog.requestId !== result.requestId) {
    return;
  }
  pairing.set(null);
  const deviceName = dialog.kind === 'show' ? dialog.deviceName : dialog.peer.name;
  onDone({ device: result.device, error: result.error, deviceName });
  if (dialog.kind === 'enter' && result.device !== null) {
    markTrusted(dialog.peer.id, true);
    // Files dropped on a device that was not paired yet go out once pairing succeeds.
    if (dialog.pendingPaths.length > 0) {
      await sendFiles(dialog.peer.id, dialog.pendingPaths);
    }
  }
}

/** Starts pairing with `peer`; `pendingPaths` are sent once it succeeds. */
export async function pairWith(peer: Peer, pendingPaths: string[] = []): Promise<void> {
  const requestId = await startPairing(peer.id);
  pairing.set({ kind: 'enter', requestId, peer, pendingPaths, submitted: false });
}

/** Sends the typed PIN. The pairing task may need a moment before it asks, so this retries briefly. */
export async function enterPin(pin: string): Promise<void> {
  const dialog = get(pairing);
  if (dialog?.kind !== 'enter') {
    return;
  }
  pairing.set({ ...dialog, submitted: true });
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (await submitPin(dialog.requestId, pin)) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/** Closes the dialog; a pairing this device started is cancelled. */
export async function closePairing(): Promise<void> {
  const dialog = get(pairing);
  pairing.set(null);
  if (dialog?.kind === 'enter' && !dialog.submitted) {
    await submitPin(dialog.requestId, null);
  }
}

/** Sends `paths` to `peer`, pairing first if the two devices are not paired yet. */
export async function sendToPeer(peer: Peer, paths: string[]): Promise<void> {
  if (paths.length === 0) {
    return;
  }
  if (peer.trusted) {
    await sendFiles(peer.id, paths);
  } else {
    await pairWith(peer, paths);
  }
}
