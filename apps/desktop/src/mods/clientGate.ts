// Holds a value that arrives later (the daemon client) and hands it to whoever asks, now or once it
// arrives, so a mod frame that starts before the app is connected waits instead of failing.

export class Gate<T> {
  private value: T | null = null;
  private waiting = new Set<(value: T) => void>();

  /** Sets (or clears, with null) the value; everyone waiting gets it at once. */
  set(value: T | null): void {
    this.value = value;
    if (value === null) return;
    const waiting = [...this.waiting];
    this.waiting.clear();
    for (const resolve of waiting) resolve(value);
  }

  /** The value now, or when it is set; rejects with `message` after `timeoutMs`. */
  get(timeoutMs: number, message: string): Promise<T> {
    if (this.value !== null) return Promise.resolve(this.value);
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(done);
        reject(new Error(message));
      }, timeoutMs);
      const done = (value: T): void => {
        clearTimeout(timer);
        resolve(value);
      };
      this.waiting.add(done);
    });
  }
}
