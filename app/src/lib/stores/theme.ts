// The appearance. Changing it applies at once: the native window, and `<html data-theme>` for the styles.

import { setTheme } from '$lib/api';

import type { Theme } from '$lib/types';

/**
 * `null` follows the operating system. The styles read `data-theme` rather than relying on `prefers-color-scheme`
 * alone: on Linux the native theme only hints GTK to prefer dark, so WebKitGTK under a dark GTK theme (Ubuntu's
 * Yaru-dark) keeps reporting dark even when light is chosen.
 */
export function applyTheme(theme: Theme | null): void {
  if (theme === null) {
    delete document.documentElement.dataset.theme;
  } else {
    document.documentElement.dataset.theme = theme;
  }
  void setTheme(theme);
}
