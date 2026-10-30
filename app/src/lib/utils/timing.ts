// Rate limiting for frequent callbacks.

/** Calls `callback` at most once per `wait` ms; the last call in a burst always runs once the wait is over. */
export function throttle<A extends unknown[]>(callback: (...args: A) => void, wait: number): (...args: A) => void {
  let last = 0;
  let pending: A | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  return (...args: A) => {
    const now = Date.now();
    const remaining = wait - (now - last);
    if (remaining <= 0) {
      last = now;
      callback(...args);
      return;
    }
    pending = args;
    timer ??= setTimeout(() => {
      timer = null;
      last = Date.now();
      if (pending !== null) {
        callback(...pending);
        pending = null;
      }
    }, remaining);
  };
}

/** Calls `callback` once calls have stopped for `wait` ms, with the last arguments. */
export function debounce<A extends unknown[]>(callback: (...args: A) => void, wait: number): (...args: A) => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return (...args: A) => {
    if (timer !== null) {
      clearTimeout(timer);
    }
    timer = setTimeout(() => {
      timer = null;
      callback(...args);
    }, wait);
  };
}
