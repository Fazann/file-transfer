// Every transfer since the app started, kept current by events, with a smoothed speed for ETA.

import { derived, writable } from 'svelte/store';

import { cancelTransfer, onEvent, pauseTransfer, resumeTransfer } from '$lib/api';

import type { Transfer, TransferProgress } from '$lib/types';

/** Weight of the newest speed sample. Lower is smoother. */
const SMOOTHING = 0.3;

export const transfers = writable<Map<number, Transfer>>(new Map());

/** Transfers still running, for the sidebar badge. */
export const activeCount = derived(transfers, (map) => [...map.values()].filter((t) => t.status === 'active').length);

/** When each transfer's last progress arrived, to compute its speed. */
const lastSample = new Map<number, { at: number; transferred: number }>();

function change(id: number, edit: (transfer: Transfer) => Transfer): void {
  transfers.update((map) => {
    const transfer = map.get(id);
    if (transfer === undefined) {
      return map;
    }
    const next = new Map(map);
    next.set(id, edit(transfer));
    return next;
  });
}

/** Applies a progress event, updating the smoothed speed. Exported for tests. */
export function applyProgress(progress: TransferProgress, now: number): void {
  const previous = lastSample.get(progress.id);
  lastSample.set(progress.id, { at: now, transferred: progress.transferred });
  change(progress.id, (transfer) => {
    let speed = transfer.speed;
    if (progress.paused) {
      speed = 0;
    } else if (previous !== undefined && now > previous.at) {
      const sample = ((progress.transferred - previous.transferred) * 1000) / (now - previous.at);
      speed = speed === 0 ? sample : speed + SMOOTHING * (sample - speed);
    }
    return {
      ...transfer,
      transferred: progress.transferred,
      size: progress.total || transfer.size,
      paused: progress.paused,
      currentFile: progress.currentFile ?? transfer.currentFile,
      speed,
    };
  });
}

/** Follows transfer events. */
export async function startTransfers(): Promise<void> {
  await Promise.all([
    onEvent('transfer-started', (started) =>
      transfers.update((map) =>
        new Map(map).set(started.id, {
          ...started,
          transferred: 0,
          paused: false,
          status: 'active',
          speed: 0,
          error: null,
          path: null,
          currentFile: null,
        }),
      ),
    ),
    onEvent('transfer-progress', (progress) => applyProgress(progress, Date.now())),
    onEvent('transfer-complete', ({ id, path }) =>
      change(id, (transfer) => ({ ...transfer, status: 'completed', transferred: transfer.size, speed: 0, path })),
    ),
    onEvent('transfer-failed', ({ id, error }) =>
      change(id, (transfer) => ({
        ...transfer,
        status: error.code === 'CANCELLED' ? 'cancelled' : 'failed',
        speed: 0,
        error,
      })),
    ),
  ]);
}

export const pause = (id: number): Promise<boolean> => pauseTransfer(id);
export const resume = (id: number): Promise<boolean> => resumeTransfer(id);
export const cancel = (id: number): Promise<boolean> => cancelTransfer(id);

/** Removes finished, failed and cancelled transfers from the list. */
export function clearFinished(): void {
  transfers.update((map) => new Map([...map].filter(([, transfer]) => transfer.status === 'active')));
}
