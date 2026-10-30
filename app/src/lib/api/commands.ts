// Typed wrappers for every Tauri command. The only place besides `events.ts` that talks to Rust.

import { invoke } from '@tauri-apps/api/core';

import type { HistoryEntry, Peer, SelfInfo, Settings, TrayLabels, TrustedDevice } from '$lib/types';

export const getSelf = (): Promise<SelfInfo> => invoke('get_self');
export const listPeers = (): Promise<Peer[]> => invoke('list_peers');

export const sendFiles = (peerId: string, paths: string[]): Promise<number> => invoke('send_files', { peerId, paths });
export const respondToOffer = (requestId: number, accept: boolean): Promise<boolean> =>
  invoke('respond_to_offer', { requestId, accept });
export const pauseTransfer = (id: number): Promise<boolean> => invoke('pause', { id });
export const resumeTransfer = (id: number): Promise<boolean> => invoke('resume', { id });
export const cancelTransfer = (id: number): Promise<boolean> => invoke('cancel', { id });
export const pauseAll = (): Promise<void> => invoke('pause_all');

export const startPairing = (peerId: string): Promise<number> => invoke('start_pairing', { peerId });
export const submitPin = (requestId: number, pin: string | null): Promise<boolean> =>
  invoke('submit_pin', { requestId, pin });
export const listTrusted = (): Promise<TrustedDevice[]> => invoke('list_trusted');
export const removeTrusted = (id: string): Promise<void> => invoke('remove_trusted', { id });
export const setAutoAccept = (id: string, autoAccept: boolean): Promise<void> =>
  invoke('set_auto_accept', { id, autoAccept });

export const getSettings = (): Promise<Settings> => invoke('get_settings');
export const updateSettings = (settings: Settings): Promise<Settings> => invoke('update_settings', { settings });
export const setTrayLabels = (labels: TrayLabels): Promise<void> => invoke('set_tray_labels', { labels });
export const exportDiagnostics = (destination: string): Promise<void> => invoke('export_diagnostics', { destination });

export const getHistory = (): Promise<HistoryEntry[]> => invoke('get_history');
export const clearHistory = (): Promise<void> => invoke('clear_history');
export const openFolder = (path: string | null): Promise<void> => invoke('open_folder', { path });
