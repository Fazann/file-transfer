// Auto-update: asks the signed manifest on GitHub Releases whether a newer version exists.

import { relaunch } from '@tauri-apps/plugin-process';
import { check } from '@tauri-apps/plugin-updater';

/** A newer version that can be installed. */
export interface AvailableUpdate {
  version: string;
  currentVersion: string;
  /** Downloads and installs the update, then restarts the app. */
  install: () => Promise<void>;
}

/** Resolves to the update if there is one. Rejects when the check fails, for example without internet access. */
export async function checkForUpdate(): Promise<AvailableUpdate | null> {
  const update = await check();
  if (update === null) {
    return null;
  }
  return {
    version: update.version,
    currentVersion: update.currentVersion,
    install: async () => {
      await update.downloadAndInstall();
      await relaunch();
    },
  };
}
