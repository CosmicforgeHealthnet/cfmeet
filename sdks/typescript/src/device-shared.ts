// Shared plumbing of the device clients (chat, calls): a tiny typed event emitter and a
// derived credential (client token, signaling credential) renewed before it expires.

import { CfMeetApiError, CfMeetError } from './errors.js';

type Handler<T> = (ev: T) => void;

/** A minimal typed event emitter (handler errors are reported to `error`, never thrown). */
export class Emitter<E extends { error: { readonly error: unknown } }> {
  readonly #handlers = new Map<keyof E, Set<Handler<never>>>();

  on<K extends keyof E>(event: K, handler: Handler<E[K]>): () => void {
    let set = this.#handlers.get(event);
    if (!set) this.#handlers.set(event, (set = new Set()));
    const h = handler as Handler<never>;
    set.add(h);
    return () => void set.delete(h);
  }

  emit<K extends keyof E>(event: K, payload: E[K]): void {
    for (const h of [...(this.#handlers.get(event) ?? [])] as Handler<E[K]>[]) {
      try {
        h(payload);
      } catch (error) {
        if (event !== 'error') this.emit('error', { error } as E['error']);
      }
    }
  }
}

/** A credential derived from the user token, with its expiry. */
export interface Expiring {
  readonly expiresAtMs: number;
}

export interface RenewingOptions {
  /** Renew this long before expiry (default 60 s; at most half the credential's lifetime). */
  readonly renewBeforeMs?: number;
  /** Renew in the background before expiry (default true); otherwise only on use. */
  readonly autoRenew?: boolean;
  readonly now?: () => number;
  /** Called after every successful exchange. */
  readonly onRenewed?: () => void;
  /** Called when a background renewal fails. */
  readonly onError?: (error: unknown) => void;
}

/** Parses an RFC 3339 timestamp into ms (throws on a malformed answer). */
export function expiryMs(value: unknown, what: string): number {
  const t = typeof value === 'string' ? Date.parse(value) : NaN;
  if (!Number.isFinite(t)) throw new CfMeetError(`the ${what} expiry is malformed`);
  return t;
}

/** True for an API refusal of a credential (401). */
export function isAuthRefusal(e: unknown): boolean {
  return e instanceof CfMeetApiError && e.status === 401;
}

/**
 * Holds a credential obtained by `exchange(previous)`: one exchange at a time, a new one
 * when it is close to expiry (on use, and in the background with `autoRenew`), and
 * `markStale()` after the server refused it.
 */
export class RenewingCredential<T extends Expiring> {
  readonly #exchange: (previous: T | null) => Promise<T>;
  readonly #renewBeforeMs: number;
  readonly #autoRenew: boolean;
  readonly #now: () => number;
  readonly #onRenewed: (() => void) | undefined;
  readonly #onError: ((error: unknown) => void) | undefined;
  #current: T | null = null;
  #obtainedAtMs = 0;
  #stale = false;
  #inflight: Promise<T> | null = null;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #closed = false;

  constructor(exchange: (previous: T | null) => Promise<T>, options: RenewingOptions = {}) {
    this.#exchange = exchange;
    this.#renewBeforeMs = options.renewBeforeMs ?? 60_000;
    this.#autoRenew = options.autoRenew ?? true;
    this.#now = options.now ?? Date.now;
    this.#onRenewed = options.onRenewed;
    this.#onError = options.onError;
  }

  /** The current credential (possibly expired), null before the first exchange. */
  get current(): T | null {
    return this.#current;
  }

  #renewAtMs(c: T): number {
    const lifetime = Math.max(0, c.expiresAtMs - this.#obtainedAtMs);
    return c.expiresAtMs - Math.min(this.#renewBeforeMs, lifetime / 2);
  }

  /** A credential that is not about to expire (exchanging when needed). */
  get(): Promise<T> {
    const c = this.#current;
    if (c && !this.#stale && this.#now() < this.#renewAtMs(c)) return Promise.resolve(c);
    return this.renew();
  }

  /** Exchanges now (concurrent callers share one exchange). */
  renew(): Promise<T> {
    if (this.#closed) return Promise.reject(new CfMeetError('the session is closed'));
    if (this.#inflight) return this.#inflight;
    const p = this.#exchange(this.#current).then((c) => {
      this.#current = c;
      this.#obtainedAtMs = this.#now();
      this.#stale = false;
      this.#schedule(c);
      this.#onRenewed?.();
      return c;
    });
    this.#inflight = p;
    const clear = () => {
      if (this.#inflight === p) this.#inflight = null;
    };
    p.then(clear, clear);
    return p;
  }

  /** The server refused the current credential: the next `get()` exchanges again. */
  markStale(): void {
    this.#stale = true;
  }

  close(): void {
    this.#closed = true;
    clearTimeout(this.#timer);
  }

  #schedule(c: T): void {
    clearTimeout(this.#timer);
    if (!this.#autoRenew || this.#closed) return;
    const delay = Math.min(2_147_483_647, Math.max(1_000, this.#renewAtMs(c) - this.#now()));
    const t = setTimeout(() => {
      this.renew().catch((e: unknown) => this.#onError?.(e));
    }, delay);
    // Never keep a server-side process alive just to renew.
    (t as { unref?: () => void }).unref?.();
    this.#timer = t;
  }
}
