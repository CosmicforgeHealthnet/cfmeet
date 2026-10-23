// Retry policy (the retry classes are computed by sdks/generate.py from the contracts):
//
//   SAFE        GET: retried on network errors, timeouts, 408 / 429 / 502 / 503 / 504.
//   IDEMPOTENT  PUT / DELETE / a mutation with an Idempotency-Key: same conditions, the
//               SAME Idempotency-Key on every attempt.
//   NO_RETRY    other mutations: retried only on 429 (a refusal: nothing was done).
//   NEVER       paid telecom mutations: never retried automatically.

import type { RetryClass } from './generated/operations.js';

export interface RetryPolicy {
  /** Retries after the first attempt (default 2). */
  readonly maxRetries: number;
  /** First backoff delay (default 500 ms), doubled per retry, with full jitter. */
  readonly initialDelayMs: number;
  /** Cap of the computed backoff (default 8 s). */
  readonly maxDelayMs: number;
  /**
   * Longest Retry-After the SDK will wait (default 30 s). A longer Retry-After (e.g. a
   * daily quota) is not waited for: the typed error is thrown at once.
   */
  readonly maxRetryAfterMs: number;
  /**
   * Waits between attempts (default: a real timer). Tests inject a fake clock
   * (`createFakeClock().sleep` from `@cfmeet/platform-sdk/testing`) so backoff and
   * Retry-After are recorded instead of slept; an aborted signal still ends the call.
   */
  readonly sleep?: (ms: number, signal?: AbortSignal) => Promise<void>;
}

export const DEFAULT_RETRY_POLICY: RetryPolicy = {
  maxRetries: 2,
  initialDelayMs: 500,
  maxDelayMs: 8_000,
  maxRetryAfterMs: 30_000,
};

const RETRYABLE_STATUS = new Set([408, 429, 502, 503, 504]);

/** Whether an HTTP status may be retried for a retry class. */
export function retryableStatus(retry: RetryClass, status: number): boolean {
  switch (retry) {
    case 'SAFE':
    case 'IDEMPOTENT':
      return RETRYABLE_STATUS.has(status);
    case 'NO_RETRY':
      return status === 429;
    case 'NEVER':
      return false;
  }
}

/** Whether a transport failure (network error, timeout) may be retried for a retry class. */
export function retryableTransport(retry: RetryClass): boolean {
  return retry === 'SAFE' || retry === 'IDEMPOTENT';
}

/**
 * Parses Retry-After (delta seconds or an HTTP date) into milliseconds; null if absent
 * or unparseable. Negative / past values become 0.
 */
export function parseRetryAfter(value: string | null, now: number = Date.now()): number | null {
  if (value == null) return null;
  const v = value.trim();
  if (v === '') return null;
  if (/^\d+(\.\d+)?$/.test(v)) return Math.round(Number(v) * 1000);
  const at = Date.parse(v);
  if (Number.isNaN(at)) return null;
  return Math.max(0, at - now);
}

/** Exponential backoff with full jitter: random in [d/2, d], d = min(max, initial·2^n). */
export function backoffDelay(policy: RetryPolicy, retryNumber: number, random: () => number = Math.random): number {
  const d = Math.min(policy.maxDelayMs, policy.initialDelayMs * 2 ** Math.max(0, retryNumber));
  return Math.round(d / 2 + random() * (d / 2));
}

/** Sleeps, rejecting early with `signal.reason` when the signal aborts. */
export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason);
    const t = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(t);
      reject(signal?.reason);
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}
