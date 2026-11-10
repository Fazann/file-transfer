// Typed subscriptions to every event the Rust side emits, plus file drops on the window.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import type {
  CommandError,
  IncomingOffer,
  PairingPin,
  PairingResult,
  Peer,
  ReceivedText,
  TransferComplete,
  TransferFailed,
  TransferProgress,
  TransferStarted,
} from '$lib/types';

/** Payload of each event, by name. */
export interface EventPayloads {
  'peer-found': Peer;
  'peer-updated': Peer;
  'peer-lost': string;
  'incoming-offer': IncomingOffer;
  'offer-closed': number;
  'transfer-started': TransferStarted;
  'transfer-progress': TransferProgress;
  'transfer-complete': TransferComplete;
  'transfer-failed': TransferFailed;
  'receive-refused': CommandError;
  'text-received': ReceivedText;
  'pairing-pin': PairingPin;
  'pairing-result': PairingResult;
}

/** Calls `handler` with each payload of `name`. Resolves to a function that stops listening. */
export function onEvent<K extends keyof EventPayloads>(
  name: K,
  handler: (payload: EventPayloads[K]) => void,
): Promise<UnlistenFn> {
  return listen<EventPayloads[K]>(name, (event) => handler(event.payload));
}

/** A drag over the window, with the position in CSS pixels. */
export type FileDrop =
  { kind: 'over'; x: number; y: number } | { kind: 'drop'; x: number; y: number; paths: string[] } | { kind: 'leave' };

/** Calls `handler` for files dragged over or dropped on the window. */
export function onFileDrop(handler: (drop: FileDrop) => void): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === 'leave') {
      handler({ kind: 'leave' });
      return;
    }
    // Tauri reports physical pixels; the page works in CSS pixels.
    const ratio = window.devicePixelRatio || 1;
    const x = payload.position.x / ratio;
    const y = payload.position.y / ratio;
    if (payload.type === 'drop') {
      handler({ kind: 'drop', x, y, paths: payload.paths });
    } else {
      handler({ kind: 'over', x, y });
    }
  });
}
