import { get } from 'svelte/store';
import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api', () => ({}));

const { activeCount, applyProgress, transfers } = await import('./transfers');

describe('transfer progress', () => {
  it('computes a smoothed speed and drops it to zero while paused', () => {
    transfers.set(
      new Map([
        [
          1,
          {
            id: 1,
            direction: 'send',
            peerId: 'p',
            peerName: 'Desk',
            fileName: 'a.bin',
            size: 100_000_000,
            transferred: 0,
            paused: false,
            status: 'active',
            speed: 0,
            error: null,
            path: null,
            currentFile: null,
            fileCount: 1,
          },
        ],
      ]),
    );
    applyProgress({ id: 1, transferred: 0, total: 100_000_000, paused: false, currentFile: null }, 0);
    applyProgress({ id: 1, transferred: 10_000_000, total: 100_000_000, paused: false, currentFile: null }, 1000);
    expect(get(transfers).get(1)?.speed).toBe(10_000_000);
    applyProgress({ id: 1, transferred: 30_000_000, total: 100_000_000, paused: false, currentFile: null }, 2000);
    expect(get(transfers).get(1)?.speed).toBe(13_000_000);
    applyProgress({ id: 1, transferred: 30_000_000, total: 100_000_000, paused: true, currentFile: null }, 3000);
    expect(get(transfers).get(1)?.speed).toBe(0);
    expect(get(activeCount)).toBe(1);
  });
});
