import { describe, expect, it } from 'vitest';

import { formatBytes, formatDate, formatDuration, formatSpeed, localeTag } from './format';

describe('formatBytes', () => {
  it('uses decimal units with sensible precision', () => {
    expect(formatBytes(0, 'en')).toBe('0B');
    expect(formatBytes(999, 'en')).toBe('999B');
    expect(formatBytes(1_500, 'en')).toBe('1.5 kB');
    expect(formatBytes(12_345_678, 'en')).toBe('12.3 MB');
    expect(formatBytes(1_500_000_000_000, 'en')).toBe('1.5 TB');
  });

  it('uses Western digits in Arabic', () => {
    expect(formatBytes(1_500_000, 'ar')).toMatch(/1[.,٫]5/);
    expect(formatBytes(1_500_000, 'ar')).not.toMatch(/[٠-٩]/);
  });
});

describe('formatSpeed', () => {
  it('shows a rate per second', () => {
    expect(formatSpeed(12_000_000, 'en')).toBe('12 MB/s');
  });
});

describe('formatDuration', () => {
  it('shows the two largest parts', () => {
    expect(formatDuration(42, 'en')).toBe('42 sec');
    expect(formatDuration(65, 'en')).toBe('1 min 5 sec');
    expect(formatDuration(3_725, 'en')).toBe('1 hr 2 min');
    expect(formatDuration(0, 'en')).toBe('0 sec');
  });
});

describe('formatDate', () => {
  it('includes the year', () => {
    expect(formatDate(new Date(2026, 9, 28, 15, 45), 'en')).toContain('2026');
  });
});

describe('localeTag', () => {
  it('asks for Latin digits in Arabic only', () => {
    expect(localeTag('ar')).toBe('ar-u-nu-latn');
    expect(localeTag('km')).toBe('km');
  });
});
