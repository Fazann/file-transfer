import { flushSync, mount, tick, unmount } from 'svelte';
import { describe, expect, it, vi } from 'vitest';

import { setupI18n } from '$lib/i18n';
import { setLocale } from '$lib/stores/locale';

import App from './App.svelte';

import type { Peer, SelfInfo, Settings } from '$lib/types';

const settings: Settings = {
  deviceName: 'Laptop',
  downloadDir: '/Users/me/Downloads/LanShare',
  port: 52000,
  streams: 4,
  bandwidthLimitMbps: 0,
  allowUntrusted: false,
  language: 'en',
  closeToTray: true,
  startAtLogin: false,
};
const me: SelfInfo = { id: 'me', name: 'Laptop', fingerprint: 'A1B2-C3D4-E5F6', port: 52000, downloadDir: '/tmp' };
const desk: Peer = { id: 'desk', name: 'Office Desk', os: 'linux', fingerprint: 'FFFF-0000-1111', trusted: false };

vi.mock('$lib/api', () => ({
  getSettings: vi.fn(async () => settings),
  getSelf: vi.fn(async () => me),
  listPeers: vi.fn(async () => [desk]),
  getHistory: vi.fn(async () => []),
  listTrusted: vi.fn(async () => []),
  setTrayLabels: vi.fn(async () => undefined),
  onEvent: vi.fn(async () => () => undefined),
  onFileDrop: vi.fn(async () => () => undefined),
}));

/** Lets the mocked promises resolve and Svelte update the page. */
async function settle(): Promise<void> {
  for (let round = 0; round < 5; round += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await tick();
  }
  flushSync();
}

describe('App', () => {
  it('renders the devices page and switches to right-to-left Arabic', async () => {
    setupI18n('en');
    const app = mount(App, { target: document.body });
    await settle();

    expect(document.body.textContent).toContain('This device');
    expect(document.body.textContent).toContain('Office Desk');
    expect(document.body.textContent).toContain('Not paired');

    setLocale('ar');
    await settle();
    expect(document.documentElement.dir).toBe('rtl');
    expect(document.documentElement.lang).toBe('ar');
    expect(document.body.textContent).toContain('الأجهزة');

    setLocale('km');
    await settle();
    expect(document.documentElement.dir).toBe('ltr');
    expect(document.body.textContent).toContain('ឧបករណ៍');
    unmount(app);
  });
});
