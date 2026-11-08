/**
 * Durable offline queue for MANAGED sends (MSG-008).
 *
 * * Every queued send keeps ONE `clientMessageId` for its whole life: a retry after a lost
 *   response is answered by Messaging as a replay of the first send (MSG-019), so a message is
 *   never duplicated however often it is retried.
 * * Sends of one conversation leave in the order they were written: only the oldest item of a
 *   conversation is due at a time.
 * * Transient failures (offline, timeouts, 429, 5xx) back off exponentially and are retried on
 *   reconnect; permanent refusals (validation, permission, idempotency conflict) stop and wait
 *   for the user to retry or discard.
 * * Stored in this browser only (localStorage, per account). Only MANAGED text is queued — the
 *   server can read it anyway; PRIVATE_E2EE plaintext is never written here. Sign-out clears it.
 *
 * Pure (no imports): node tests run this file directly (tests/messaging-outbox.test.mjs).
 */

export type OutboxState = "queued" | "sending" | "failed";

export type OutboxItem = {
  key: string;
  conversationId: string;
  clientMessageId: string;
  text: string;
  replyToMessageId: string | null;
  mentionedPrincipalIds: string[];
  createdAt: string;
  attempts: number;
  state: OutboxState;
  error: string | null;
  nextAttemptAt: number;
};

export interface OutboxStorage {
  load(): string | null;
  save(value: string): void;
  clear(): void;
}

export const OUTBOX_KEY_PREFIX = "cfmeet.messaging.outbox.";
const MAX_ITEMS = 200;
const MAX_BACKOFF_MS = 60_000;

/** Transient failures are retried; everything else waits for the user. */
export function isRetryable(status: number, code?: string | null): boolean {
  if (code === "IDEMPOTENCY_KEY_REUSED" || code === "VALIDATION_ERROR") return false;
  return status === 0 || status === 408 || status === 425 || status === 429 || status >= 500;
}

export function backoffMs(attempts: number): number {
  return Math.min(MAX_BACKOFF_MS, 1000 * 2 ** Math.max(0, attempts - 1));
}

function valid(item: unknown): item is OutboxItem {
  if (!item || typeof item !== "object") return false;
  const i = item as Record<string, unknown>;
  return (
    typeof i.key === "string" &&
    typeof i.conversationId === "string" &&
    typeof i.clientMessageId === "string" &&
    typeof i.text === "string" &&
    typeof i.attempts === "number" &&
    (i.state === "queued" || i.state === "sending" || i.state === "failed")
  );
}

export class MessageOutbox {
  private items: OutboxItem[] = [];
  private readonly listeners = new Set<() => void>();

  constructor(
    private readonly storage: OutboxStorage | null,
    private readonly now: () => number = () => Date.now(),
  ) {
    this.items = this.read();
  }

  private read(): OutboxItem[] {
    try {
      const raw = this.storage?.load();
      if (!raw) return [];
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) return [];
      // A send interrupted by a reload is due again (same clientMessageId: no duplicate).
      return parsed.filter(valid).map((i) => (i.state === "sending" ? { ...i, state: "queued" as const } : i));
    } catch {
      return [];
    }
  }

  private persist(): void {
    try {
      if (this.items.length) this.storage?.save(JSON.stringify(this.items));
      else this.storage?.clear();
    } catch {
      // Storage full or unavailable: the queue still works for this page's lifetime.
    }
    for (const l of this.listeners) l();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  all(conversationId?: string): OutboxItem[] {
    return this.items.filter((i) => !conversationId || i.conversationId === conversationId);
  }

  enqueue(input: { key: string; conversationId: string; clientMessageId: string; text: string; replyToMessageId?: string | null; mentionedPrincipalIds?: string[] }): OutboxItem {
    if (this.items.length >= MAX_ITEMS) throw new Error("OUTBOX_FULL");
    const item: OutboxItem = {
      key: input.key,
      conversationId: input.conversationId,
      clientMessageId: input.clientMessageId,
      text: input.text,
      replyToMessageId: input.replyToMessageId ?? null,
      mentionedPrincipalIds: input.mentionedPrincipalIds ?? [],
      createdAt: new Date(this.now()).toISOString(),
      attempts: 0,
      state: "queued",
      error: null,
      nextAttemptAt: 0,
    };
    this.items.push(item);
    this.persist();
    return item;
  }

  /** The items to send now: per conversation only the oldest, and only once it is due. */
  due(): OutboxItem[] {
    const now = this.now();
    const seen = new Set<string>();
    const out: OutboxItem[] = [];
    for (const i of this.items) {
      if (seen.has(i.conversationId)) continue;
      seen.add(i.conversationId);
      // A failed head blocks its conversation (order is kept) until retried or discarded.
      if (i.state === "queued" && i.nextAttemptAt <= now) out.push(i);
    }
    return out;
  }

  private update(key: string, patch: Partial<OutboxItem>): void {
    this.items = this.items.map((i) => (i.key === key ? { ...i, ...patch } : i));
    this.persist();
  }

  markSending(key: string): void {
    const i = this.items.find((x) => x.key === key);
    if (i) this.update(key, { state: "sending", attempts: i.attempts + 1 });
  }

  /** The server accepted the send (or answered it as a replay). */
  markSent(key: string): void {
    this.items = this.items.filter((i) => i.key !== key);
    this.persist();
  }

  markFailed(key: string, error: string, retryable: boolean): void {
    const i = this.items.find((x) => x.key === key);
    if (!i) return;
    if (retryable) this.update(key, { state: "queued", error, nextAttemptAt: this.now() + backoffMs(i.attempts) });
    else this.update(key, { state: "failed", error });
  }

  /** Removes queued items the server already holds (seen by clientMessageId in a sync). */
  confirm(conversationId: string, clientMessageIds: Iterable<string>): number {
    const ids = new Set(clientMessageIds);
    const before = this.items.length;
    this.items = this.items.filter((i) => !(i.conversationId === conversationId && ids.has(i.clientMessageId) && i.state !== "sending"));
    if (this.items.length !== before) this.persist();
    return before - this.items.length;
  }

  /** The user retries a failed send (same clientMessageId). */
  retry(key: string): void {
    this.update(key, { state: "queued", error: null, nextAttemptAt: 0 });
  }

  /** Reconnected: everything waiting on a back-off is due now. */
  wake(): void {
    if (this.items.some((i) => i.state === "queued" && i.nextAttemptAt > 0)) {
      this.items = this.items.map((i) => (i.state === "queued" ? { ...i, nextAttemptAt: 0 } : i));
      this.persist();
    }
  }

  discard(key: string): void {
    this.markSent(key);
  }

  /** A conversation this account left / lost: its queued sends can never be delivered. */
  dropConversation(conversationId: string): void {
    const before = this.items.length;
    this.items = this.items.filter((i) => i.conversationId !== conversationId);
    if (before !== this.items.length) this.persist();
  }

  clear(): void {
    this.items = [];
    this.persist();
  }
}

/**
 * Sends every due item, one at a time per conversation, oldest first; keeps going while
 * sends succeed. `send` throws `{ status, code }`-shaped errors for refusals.
 */
export async function flushOutbox(
  outbox: MessageOutbox,
  send: (item: OutboxItem) => Promise<void>,
  maxRounds = 50,
): Promise<number> {
  let sent = 0;
  for (let round = 0; round < maxRounds; round++) {
    const due = outbox.due();
    if (!due.length) break;
    let progressed = false;
    for (const item of due) {
      outbox.markSending(item.key);
      try {
        await send(item);
        outbox.markSent(item.key);
        sent++;
        progressed = true;
      } catch (e) {
        const err = e as { status?: number; code?: string; message?: string } | null;
        const status = typeof err?.status === "number" ? err.status : 0;
        outbox.markFailed(item.key, err?.code ?? err?.message ?? "SEND_FAILED", isRetryable(status, err?.code));
      }
    }
    if (!progressed) break;
  }
  return sent;
}

/** localStorage-backed storage for one account (null outside the browser). */
export function browserOutboxStorage(principalId: string): OutboxStorage | null {
  if (typeof window === "undefined" || !window.localStorage) return null;
  const key = OUTBOX_KEY_PREFIX + principalId;
  return {
    load: () => window.localStorage.getItem(key),
    save: (v) => window.localStorage.setItem(key, v),
    clear: () => window.localStorage.removeItem(key),
  };
}

/** Sign-out: every account's queued MANAGED text leaves this browser. */
export function clearAllOutboxes(): void {
  if (typeof window === "undefined" || !window.localStorage) return;
  const keys: string[] = [];
  for (let i = 0; i < window.localStorage.length; i++) {
    const k = window.localStorage.key(i);
    if (k?.startsWith(OUTBOX_KEY_PREFIX)) keys.push(k);
  }
  for (const k of keys) window.localStorage.removeItem(k);
}
