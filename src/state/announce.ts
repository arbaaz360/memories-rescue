/**
 * Progress announcements for screen readers (NFR-USE-03): at most one every
 * 10 seconds, however often the engine reports. State changes (paused, done)
 * are announced at once and do not count against the interval.
 */

export const PROGRESS_ANNOUNCE_INTERVAL_MS = 10_000;

export class AnnounceThrottle {
  private last: number | null = null;

  constructor(private readonly intervalMs = PROGRESS_ANNOUNCE_INTERVAL_MS) {}

  /** True when a progress message may be announced now; records the time when it says yes. */
  allow(now: number): boolean {
    if (this.last !== null && now - this.last < this.intervalMs) return false;
    this.last = now;
    return true;
  }

  reset(): void {
    this.last = null;
  }
}
