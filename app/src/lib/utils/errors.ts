// Turns a `CommandError` into a translated sentence.

import { formatBytes } from './format';

import type { CommandError, Locale } from '$lib/types';

/** svelte-i18n's message function, as far as this module needs it. */
export type Translate = (key: string, options?: { values?: Record<string, string | number> }) => string;

/** Params that hold a number of bytes. */
const BYTE_PARAMS = ['needed', 'available', 'maxBytes'];

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
      values[name] = BYTE_PARAMS.includes(name) ? formatBytes(value, locale) : value;
    } else if (typeof value === 'string') {
      values[name] = value;
    }
  }
  const message = translate(`errors.${error.code}`, { values });
  const diagnosis = connectDiagnosis(translate, error, values);
  if (diagnosis) {
    return `${message} ${diagnosis}`;
  }
  // Nothing was tried, so nothing says why. On macOS a missing permission is a likely cause, so name it.
  return LOCAL_NETWORK_CODES.includes(error.code) && isMacos()
    ? `${message} ${translate('errors.macos_local_network')}`
    : message;
}

/**
 * Why a connection failed and what to do, from the `quic`, `tcp`, `port` and `hint` params the backend adds after
 * trying to connect, e.g. "QUIC: timed out. TCP: no route to host. macOS is blocking…". Empty without them.
 */
function connectDiagnosis(translate: Translate, error: CommandError, values: Record<string, string | number>): string {
  const parts: string[] = [];
  for (const transport of ['quic', 'tcp']) {
    const failure = error.params[transport];
    if (typeof failure === 'string') {
      const outcome = { failure: translate(`errors.connect.failure.${failure}`) };
      parts.push(translate(`errors.connect.${transport}`, { values: outcome }));
    }
  }
  const hint = error.params.hint;
  if (typeof hint === 'string') {
    parts.push(translate(`errors.connect.hint.${hint}`, { values }));
  }
  return parts.join(' ');
}
