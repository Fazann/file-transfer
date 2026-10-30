// Turns a `CommandError` into a translated sentence.

import { formatBytes } from './format';

import type { CommandError, Locale } from '$lib/types';

/** svelte-i18n's message function, as far as this module needs it. */
export type Translate = (key: string, options?: { values?: Record<string, string | number> }) => string;

/** Errors that are what macOS shows when LanShare has no Local Network permission. */
const LOCAL_NETWORK_CODES = ['PEER_UNREACHABLE', 'TRANSPORT_UNAVAILABLE'];

function isMacos(): boolean {
  return /Mac OS X|Macintosh/.test(navigator.userAgent);
}

/**
 * The translated message for `error`, with `context` (such as `device`) for placeholders the error itself does not
 * carry. Byte counts in the params are formatted for `locale`.
 */
export function errorMessage(
  translate: Translate,
  error: CommandError,
  locale: Locale,
  context: Record<string, string> = {},
): string {
  const values: Record<string, string | number> = { device: '', ...context };
  for (const [name, value] of Object.entries(error.params)) {
    if (typeof value === 'number') {
      values[name] = name === 'needed' || name === 'available' ? formatBytes(value, locale) : value;
    } else if (typeof value === 'string') {
      values[name] = value;
    }
  }
  const message = translate(`errors.${error.code}`, { values });
  // macOS gives no way to tell a refused permission from an unreachable device, so name it as a possible cause.
  return LOCAL_NETWORK_CODES.includes(error.code) && isMacos()
    ? `${message} ${translate('errors.macos_local_network')}`
    : message;
}
