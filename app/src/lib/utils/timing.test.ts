import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { debounce, throttle } from './timing';

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('throttle', () => {
  it('runs at once, then at most once per wait, ending with the last call', () => {
    const calls: number[] = [];
    const throttled = throttle((value: number) => calls.push(value), 100);
    throttled(1);
    throttled(2);
    throttled(3);
    expect(calls).toEqual([1]);
    vi.advanceTimersByTime(100);
    expect(calls).toEqual([1, 3]);
  });
});

describe('debounce', () => {
  it('runs once after the calls stop', () => {
    const calls: string[] = [];
    const debounced = debounce((value: string) => calls.push(value), 50);
    debounced('a');
    vi.advanceTimersByTime(30);
    debounced('b');
    vi.advanceTimersByTime(30);
    expect(calls).toEqual([]);
    vi.advanceTimersByTime(20);
    expect(calls).toEqual(['b']);
  });
});
