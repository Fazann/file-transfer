import { describe, expect, it } from 'vitest';

import errorCodes from './error-codes.json';
import ar from './locales/ar.json';
import en from './locales/en.json';
import km from './locales/km.json';

/** Every key path in a nested message object, e.g. `nav.devices`. */
function keys(messages: object, prefix = ''): string[] {
  return Object.entries(messages).flatMap(([key, value]) =>
    typeof value === 'object' && value !== null ? keys(value, `${prefix}${key}.`) : [`${prefix}${key}`],
  );
}

/** The `{placeholder}` names in a message, sorted. */
function placeholders(message: string): string[] {
  return [...message.matchAll(/\{(\w+)/g)].map((match) => match[1] ?? '').sort();
}

function lookup(messages: object, path: string): string {
  const value = path.split('.').reduce<unknown>((node, key) => (node as Record<string, unknown>)[key], messages);
  return String(value);
}

describe('translations', () => {
  const english = keys(en);

  it.each([
    ['km', km],
    ['ar', ar],
  ])('%s has exactly the English keys', (_name, messages) => {
    expect(keys(messages).sort()).toEqual([...english].sort());
  });

  it.each([
    ['km', km],
    ['ar', ar],
  ])('%s uses the same placeholders as English', (_name, messages) => {
    for (const key of english) {
      expect(placeholders(lookup(messages, key)), key).toEqual(placeholders(lookup(en, key)));
    }
  });

  it('has a message for every error code from the Rust side', () => {
    expect(errorCodes.length).toBeGreaterThan(0);
    for (const code of errorCodes) {
      expect(english, code).toContain(`errors.${code}`);
    }
  });

  it('has no empty messages', () => {
    for (const messages of [en, km, ar]) {
      for (const key of keys(messages)) {
        expect(lookup(messages, key).trim(), key).not.toBe('');
      }
    }
  });
});
