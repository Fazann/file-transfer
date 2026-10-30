// Native dialogs and notifications. Text is passed in already translated.

import { open, save } from '@tauri-apps/plugin-dialog';
import { isPermissionGranted, requestPermission, sendNotification } from '@tauri-apps/plugin-notification';

/** Lets the user pick files. Resolves to their paths, or an empty list if cancelled. */
export async function pickFiles(title: string): Promise<string[]> {
  const picked = await open({ multiple: true, directory: false, title });
  if (picked === null) {
    return [];
  }
  return Array.isArray(picked) ? picked : [picked];
}

/** Lets the user pick a folder. Resolves to its path, or `null` if cancelled. */
export async function pickFolder(title: string): Promise<string | null> {
  const picked = await open({ multiple: false, directory: true, title });
  return typeof picked === 'string' ? picked : null;
}

/** Lets the user choose where to save a file. Resolves to its path, or `null` if cancelled. */
export function pickSavePath(title: string, defaultPath: string): Promise<string | null> {
  return save({ title, defaultPath });
}

/** Shows an operating system notification, asking for permission the first time. */
export async function notify(title: string, body: string): Promise<void> {
  let granted = await isPermissionGranted();
  if (!granted) {
    granted = (await requestPermission()) === 'granted';
  }
  if (granted) {
    sendNotification({ title, body });
  }
}
