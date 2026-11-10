import { get } from 'svelte/store';
import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/api', () => ({}));

const { closeReceivedText, receivedTexts, utf8Length } = await import('./texts');

describe('texts', () => {
  it('counts the size limit in UTF-8 bytes, like the Rust side', () => {
    expect(utf8Length('abc')).toBe(3);
    expect(utf8Length('é')).toBe(2);
    expect(utf8Length('ខ្មែរ')).toBe(15);
    expect(utf8Length('😀')).toBe(4);
  });

  it('shows received texts oldest first', () => {
    const text = (body: string) => ({ deviceId: 'd', deviceName: 'Desk', fingerprint: 'F', text: body });
    receivedTexts.set([text('first'), text('second')]);
    closeReceivedText();
    expect(get(receivedTexts).map((received) => received.text)).toEqual(['second']);
    closeReceivedText();
    expect(get(receivedTexts)).toEqual([]);
  });
});
