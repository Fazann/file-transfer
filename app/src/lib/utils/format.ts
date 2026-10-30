// Locale-aware formatting of sizes, speeds, durations and dates. Every number shown in the UI goes through here.

import type { Locale } from '$lib/types';

const UNITS = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte'] as const;

/** The BCP 47 tag for a UI locale. Arabic uses Western digits everywhere, for consistency with file names and IPs. */
export function localeTag(locale: Locale | string): string {
  return locale === 'ar' ? 'ar-u-nu-latn' : locale;
}

/** Picks a decimal unit (1 kB = 1000 bytes) so the number stays below 1000. */
function scale(bytes: number): { value: number; unit: (typeof UNITS)[number] } {
  let value = Math.max(0, bytes);
  let index = 0;
  while (value >= 1000 && index < UNITS.length - 1) {
    value /= 1000;
    index += 1;
  }
  return { value, unit: UNITS[index] ?? 'byte' };
}

function digits(value: number, unit: string): number {
  if (unit === 'byte') {
    return 0;
  }
  return value < 10 ? 2 : value < 100 ? 1 : 0;
}

/** For example `1.5 GB`, with the unit name in the given language. */
export function formatBytes(bytes: number, locale: Locale | string): string {
  const { value, unit } = scale(bytes);
  return new Intl.NumberFormat(localeTag(locale), {
    style: 'unit',
    unit,
    // `short` would print `999 byte`; `narrow` gives `999B`, in line with `kB` and `MB`.
    unitDisplay: unit === 'byte' ? 'narrow' : 'short',
    maximumFractionDigits: digits(value, unit),
  }).format(value);
}

/** For example `12 MB/s`. */
export function formatSpeed(bytesPerSecond: number, locale: Locale | string): string {
  const { value, unit } = scale(bytesPerSecond);
  return new Intl.NumberFormat(localeTag(locale), {
    style: 'unit',
    unit: `${unit}-per-second`,
    unitDisplay: 'short',
    maximumFractionDigits: digits(value, unit),
  }).format(value);
}

/** For example `1 hr 5 min` or `42 sec`: at most the two largest parts, rounded to whole seconds. */
export function formatDuration(seconds: number, locale: Locale | string): string {
  const total = Math.max(0, Math.round(seconds));
  const parts: [number, 'hour' | 'minute' | 'second'][] = [
    [Math.floor(total / 3600), 'hour'],
    [Math.floor((total % 3600) / 60), 'minute'],
    [total % 60, 'second'],
  ];
  const shown = parts.filter(([value]) => value > 0).slice(0, 2);
  const list = shown.length > 0 ? shown : [[0, 'second'] as [number, 'second']];
  const tag = localeTag(locale);
  return list
    .map(([value, unit]) => new Intl.NumberFormat(tag, { style: 'unit', unit, unitDisplay: 'short' }).format(value))
    .join(' ');
}

/** Date and time, for example `Oct 28, 2026, 3:45 PM`. */
export function formatDate(date: Date | number, locale: Locale | string): string {
  return new Intl.DateTimeFormat(localeTag(locale), { dateStyle: 'medium', timeStyle: 'short' }).format(date);
}
