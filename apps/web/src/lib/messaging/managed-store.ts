/**
 * MANAGED conversation state of the web client (MSG-005 / MSG-008): a message store keyed by
 * `messageId` that converges on the server's CURRENT representation, not an append-only log.
 *
 * * Messages arrive from three places — forward sync (`afterSequence`), back-pagination
 *   (`beforeSequence`) and durable conversation events (realtime frames or the REST replay
 *   `GET /v1/conversations/{id}/events?after=`). Message events carry the message's current
 *   representation (Messaging `hydrate`), so an edit replaces the text and a deletion turns
 *   the message into a tombstone wherever it is in the loaded window.
 * * A tombstone is final: a later (stale) ACTIVE copy never resurrects it, and a lower
 *   `editCount` never overwrites a newer edit.
 * * Reaction / pin / membership events only say "refetch this"; the caller does it.
 *
 * Pure (no imports): node tests run this file directly (tests/messaging-managed-sync.test.mjs).
 */

export type MessageLifecycle = "ACTIVE" | "DELETED" | "MODERATED" | "EXPIRED" | "PURGED" | "HIDDEN_FOR_YOU";

/** The Messaging contract `Message` (fields the web client reads). */
export type ServerMessage = {
  messageId: string;
  conversationId: string;
  sequence: number;
  kind: "APPLICATION" | "MLS_COMMIT" | "EDIT";
  securityProfile: "MANAGED" | "PRIVATE_E2EE";
  senderPrincipalId: string;
  senderDeviceId?: string | null;
  clientMessageId: string;
  targetMessageId?: string | null;
  lifecycle: MessageLifecycle | string;
  lifecycleReason?: string | null;
  editCount?: number;
  editedAt?: string | null;
  createdAt: string;
  attachmentIds: string[];
  e2ee?: { epoch: number; ciphertext: string | null } | null;
  managed?: { text: string | null; presentation?: "STANDARD" | "ANNOUNCEMENT" } | null;
  replyToMessageId?: string | null;
  threadRootId?: string | null;
};

export type ManagedRecord = {
  messageId: string;
  sequence: number;
  senderPrincipalId: string;
  clientMessageId: string;
  createdAt: string;
  lifecycle: MessageLifecycle;
  text: string | null;
  editCount: number;
  editedAt: string | null;
  attachmentIds: string[];
  replyToMessageId: string | null;
  threadRootId: string | null;
  announcement: boolean;
};

/** A durable conversation event (`ConversationEvent`). */
export type ConversationEvent = {
  eventId?: string;
  type: string;
  conversationId: string;
  eventSequence: number;
  occurredAt?: string;
  data?: Record<string, unknown>;
};

/** What the caller must refetch after applying events. */
export type Refresh = {
  reactions: Set<string>;
  pins: boolean;
  conversation: boolean;
  /** Messages that changed (edited / tombstoned / new) — e.g. to refresh attachment metadata. */
  messages: Set<string>;
};

export const emptyRefresh = (): Refresh => ({ reactions: new Set(), pins: false, conversation: false, messages: new Set() });

const LIFECYCLES = new Set<string>(["ACTIVE", "DELETED", "MODERATED", "EXPIRED", "PURGED", "HIDDEN_FOR_YOU"]);
const TOMBSTONE_EVENTS: Record<string, MessageLifecycle> = {
  "message.deleted": "DELETED",
  "message.moderated": "MODERATED",
  "message.expired": "EXPIRED",
  "message.purged": "PURGED",
};

function lifecycleOf(value: unknown): MessageLifecycle {
  return typeof value === "string" && LIFECYCLES.has(value) ? (value as MessageLifecycle) : "DELETED";
}

function isServerMessage(value: unknown): value is ServerMessage {
  if (!value || typeof value !== "object") return false;
  const m = value as Record<string, unknown>;
  return typeof m.messageId === "string" && Number.isSafeInteger(m.sequence) && typeof m.kind === "string";
}

export function toRecord(m: ServerMessage): ManagedRecord {
  const lifecycle = lifecycleOf(m.lifecycle);
  return {
    messageId: m.messageId,
    sequence: m.sequence,
    senderPrincipalId: m.senderPrincipalId,
    clientMessageId: m.clientMessageId,
    createdAt: m.createdAt,
    lifecycle,
    // A tombstone never shows content, whatever a stale copy carried.
    text: lifecycle === "ACTIVE" ? (m.managed?.text ?? null) : null,
    editCount: m.editCount ?? 0,
    editedAt: m.editedAt ?? null,
    attachmentIds: lifecycle === "ACTIVE" ? [...(m.attachmentIds ?? [])] : [],
    replyToMessageId: m.replyToMessageId ?? null,
    threadRootId: m.threadRootId ?? null,
    announcement: m.managed?.presentation === "ANNOUNCEMENT",
  };
}

/** Whether `next` (a representation of the same message) should replace `current`. */
export function supersedes(current: ManagedRecord, next: ManagedRecord): boolean {
  if (current.lifecycle !== "ACTIVE") return false; // tombstones are final
  if (next.lifecycle !== "ACTIVE") return true;
  return next.editCount >= current.editCount;
}

function sameRecord(a: ManagedRecord, b: ManagedRecord): boolean {
  return (
    a.lifecycle === b.lifecycle &&
    a.text === b.text &&
    a.editCount === b.editCount &&
    a.attachmentIds.join() === b.attachmentIds.join() &&
    a.replyToMessageId === b.replyToMessageId
  );
}

export class ManagedConversationState {
  private readonly records = new Map<string, ManagedRecord>();
  /** Highest message sequence seen (the forward `afterSequence` cursor). */
  maxSequence = 0;
  /** Oldest loaded sequence (`beforeSequence` for the next older page); null: nothing loaded. */
  minSequence: number | null = null;
  /** More history exists before `minSequence`. */
  hasOlder = false;
  /** Durable event position applied so far (null: unknown, the server's ack cursor decides). */
  eventCursor: number | null = null;
  /** Other members' read positions (receipts), by principal. */
  readonly readBy = new Map<string, number>();
  /** Hidden for this account (Hub clear-for-me watermark). */
  clearedThroughSequence = 0;

  get size(): number {
    return this.records.size;
  }

  get(messageId: string): ManagedRecord | undefined {
    return this.records.get(messageId);
  }

  byClientMessageId(clientMessageId: string): ManagedRecord | undefined {
    for (const r of this.records.values()) if (r.clientMessageId === clientMessageId) return r;
    return undefined;
  }

  /** Upserts a page of messages; returns the ids that changed. */
  applyMessages(messages: readonly ServerMessage[]): Set<string> {
    const changed = new Set<string>();
    for (const m of messages) {
      if (!isServerMessage(m)) continue;
      this.maxSequence = Math.max(this.maxSequence, m.sequence);
      if (m.kind === "MLS_COMMIT") continue;
      if (m.kind === "EDIT") {
        // A legacy edit record: apply its text to the target (never shown on its own).
        const target = m.targetMessageId ? this.records.get(m.targetMessageId) : undefined;
        if (target && target.lifecycle === "ACTIVE" && lifecycleOf(m.lifecycle) === "ACTIVE" && m.managed?.text != null) {
          this.records.set(target.messageId, { ...target, text: m.managed.text, editCount: target.editCount + 1, editedAt: m.createdAt });
          changed.add(target.messageId);
        }
        continue;
      }
      const next = toRecord(m);
      const current = this.records.get(m.messageId);
      if (!current) {
        this.records.set(m.messageId, next);
        this.minSequence = this.minSequence === null ? m.sequence : Math.min(this.minSequence, m.sequence);
        changed.add(m.messageId);
      } else if (supersedes(current, next) && !sameRecord(current, next)) {
        this.records.set(m.messageId, next);
        changed.add(m.messageId);
      }
    }
    return changed;
  }

  /** Marks an older page as loaded (back-pagination). */
  applyOlderPage(messages: readonly ServerMessage[], nextBeforeSequence: number | null | undefined): Set<string> {
    const changed = this.applyMessages(messages);
    this.hasOlder = nextBeforeSequence != null && nextBeforeSequence > 1;
    return changed;
  }

  /**
   * Applies durable events in order. Events at or below the cursor are duplicates and
   * ignored; the cursor only moves forward.
   */
  applyEvents(events: readonly ConversationEvent[], refresh: Refresh = emptyRefresh()): Refresh {
    const ordered = [...events].sort((a, b) => a.eventSequence - b.eventSequence);
    for (const e of ordered) {
      if (!Number.isSafeInteger(e.eventSequence)) continue;
      if (this.eventCursor !== null && e.eventSequence <= this.eventCursor) continue;
      this.applyEvent(e, refresh);
      this.eventCursor = Math.max(this.eventCursor ?? 0, e.eventSequence);
    }
    return refresh;
  }

  private applyEvent(e: ConversationEvent, refresh: Refresh): void {
    const data = e.data ?? {};
    const message = isServerMessage(data.message) ? (data.message as ServerMessage) : null;
    const messageId = message?.messageId ?? (typeof data.messageId === "string" ? data.messageId : null);
    const type = e.type;
    if (type.startsWith("message.")) {
      if (message) {
        for (const id of this.applyMessages([message])) refresh.messages.add(id);
      } else if (messageId && TOMBSTONE_EVENTS[type]) {
        // The message is outside this member's visible window or not hydrated: tombstone
        // what we hold, never keep its old content.
        const current = this.records.get(messageId);
        if (current && current.lifecycle === "ACTIVE") {
          this.records.set(messageId, { ...current, lifecycle: TOMBSTONE_EVENTS[type], text: null, attachmentIds: [] });
          refresh.messages.add(messageId);
        }
      }
      if (type === "message.created" && !message && typeof data.sequence === "number") {
        // Not hydrated (outside the window): the forward sync fetches it.
        refresh.conversation = refresh.conversation || data.sequence > this.maxSequence;
      }
      return;
    }
    if (type.startsWith("reaction.")) {
      if (messageId) refresh.reactions.add(messageId);
      return;
    }
    if (type.startsWith("pin.")) {
      refresh.pins = true;
      return;
    }
    if (type === "read.cursor.updated") {
      const principal = typeof data.principalId === "string" ? data.principalId : null;
      const seq = typeof data.readSequence === "number" ? data.readSequence : null;
      if (principal && seq !== null) this.readBy.set(principal, Math.max(this.readBy.get(principal) ?? 0, seq));
      return;
    }
    if (type.startsWith("member.") || type.startsWith("conversation.") || type.startsWith("policy.")) {
      refresh.conversation = true;
    }
  }

  /** Records ordered by sequence, without what this account cleared for itself. */
  ordered(): ManagedRecord[] {
    return [...this.records.values()]
      .filter((r) => r.sequence > this.clearedThroughSequence)
      .sort((a, b) => a.sequence - b.sequence);
  }

  /** Principals (other than `me`) whose read position reached `sequence`. */
  seenBy(sequence: number, me: string | null): string[] {
    const out: string[] = [];
    for (const [p, s] of this.readBy) if (p !== me && s >= sequence) out.push(p);
    return out;
  }
}

// ------------------------------------------------------------------------- timeline

/** A Hub timeline card (`TimelineEntry`): calls and meeting invitations only. */
export type TimelineCard = {
  entryId: string;
  kind: "CALL" | "MEETING_INVITATION" | string;
  occurredAt: string;
  anchorSequence?: number | null;
  conversationId?: string;
  sourceId?: string;
  call?: Record<string, unknown> | null;
  meeting?: Record<string, unknown> | null;
};

export type TimelineItem<M> = { type: "message"; key: string; sequence: number | null; at: string | null; message: M } | { type: "card"; key: string; sequence: number | null; at: string; card: TimelineCard };

/**
 * Merges messages with Hub cards per the timeline contract: by `anchorSequence` (a card sits
 * after the message with that sequence), then `occurredAt`, then `entryId`. Cards at or below
 * the clear-for-me watermark are hidden. Messages without a sequence (still sending) stay last.
 */
export function mergeTimeline<M extends { key: string; sequence: number | null; createdAt: string | null }>(
  messages: readonly M[],
  cards: readonly TimelineCard[],
  clearedThroughSequence = 0,
): TimelineItem<M>[] {
  const items: (TimelineItem<M> & { rank: [number, number, string, string] })[] = [];
  const INF = Number.MAX_SAFE_INTEGER;
  for (const m of messages) {
    const seq = m.sequence ?? INF;
    items.push({ type: "message", key: m.key, sequence: m.sequence, at: m.createdAt, message: m, rank: [seq, 0, m.createdAt ?? "", m.key] });
  }
  for (const c of cards) {
    const anchor = c.anchorSequence ?? null;
    if (anchor !== null && anchor <= clearedThroughSequence && clearedThroughSequence > 0) continue;
    // A card without an anchor is placed by time among the messages.
    const seq = anchor ?? sequenceAt(messages, c.occurredAt);
    items.push({ type: "card", key: `card:${c.entryId}`, sequence: anchor, at: c.occurredAt, card: c, rank: [seq, 1, c.occurredAt, c.entryId] });
  }
  items.sort((a, b) => a.rank[0] - b.rank[0] || a.rank[1] - b.rank[1] || a.rank[2].localeCompare(b.rank[2]) || a.rank[3].localeCompare(b.rank[3]));
  return items.map((item) => {
    const { rank: _rank, ...rest } = item;
    void _rank;
    return rest as TimelineItem<M>;
  });
}

/** The sequence of the last message created at or before `at` (0: before every message). */
function sequenceAt(messages: readonly { sequence: number | null; createdAt: string | null }[], at: string): number {
  let seq = 0;
  for (const m of messages) if (m.sequence !== null && m.createdAt && m.createdAt <= at) seq = Math.max(seq, m.sequence);
  return seq;
}
