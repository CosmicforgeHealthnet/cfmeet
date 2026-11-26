import type { StoredRecord } from "./crypto-client";

export type PrivateHistoryPage = { records: StoredRecord[]; nextBeforeSequence: number | null };
/** Raw page coverage includes controls, so invisible pages never terminate history. */
export class PrivateHistoryWindow {
  private records = new Map<number, StoredRecord>();
  private ranges: { low: number; high: number }[] = [];
  private _head = 0;
  private first = 1;

  /** Newest sequence merged from a head (refresh) page; 0 before the first. */
  get head(): number { return this._head; }
  get ordered(): StoredRecord[] { return [...this.records.values()].sort((a, b) => a.sequence - b.sequence); }
  get cursor(): number | null {
    const range = this.ranges.find(item => item.high >= this._head + 1);
    return range && range.low > this.first ? range.low : null;
  }
  upsert(records: StoredRecord[]): void {
    for (const record of records) {
      if (record.sequence < this.first) continue;
      const previous = this.records.get(record.sequence);
      if (previous?.messageId && record.messageId && previous.messageId !== record.messageId) throw new Error("PRIVATE_HISTORY_CONFLICT");
      if (previous?.status === "REMOVED" && record.status !== "REMOVED") continue;
      this.records.set(record.sequence, record);
    }
  }
  /** Replay updates loaded rows without implicitly loading the complete archive. */
  updateKnown(records: StoredRecord[]): void {
    this.upsert(records.filter(record => this.records.has(record.sequence)));
  }
  merge(page: PrivateHistoryPage, before?: number): void {
    if (page.records.length > 200) throw new Error("PRIVATE_HISTORY_PAGE_INVALID");
    const oldest = page.records[0]?.sequence;
    const newest = page.records.at(-1)?.sequence;
    if (page.records.some((record, index) => !Number.isSafeInteger(record.sequence) || record.sequence < this.first || (index > 0 && record.sequence <= page.records[index - 1].sequence) || (before !== undefined && record.sequence >= before))) throw new Error("PRIVATE_HISTORY_PAGE_INVALID");
    if (page.nextBeforeSequence !== null && (oldest === undefined || page.nextBeforeSequence !== oldest)) throw new Error("PRIVATE_HISTORY_PAGE_INVALID");
    if (before === undefined) {
      if (newest === undefined) { this.records.clear(); this.ranges = []; this._head = 0; return; }
      if (newest < this._head) throw new Error("PRIVATE_HISTORY_HEAD_REGRESSION");
      this._head = newest;
    }
    this.upsert(page.records);
    const low = page.nextBeforeSequence === null ? this.first : oldest!;
    const high = before ?? (newest === undefined ? this.first : newest + 1);
    if (high > low) this.ranges.push({ low, high });
    const merged: { low: number; high: number }[] = [];
    for (const range of this.ranges.sort((a, b) => a.low - b.low)) {
      const previous = merged.at(-1);
      if (previous && previous.high >= range.low) previous.high = Math.max(previous.high, range.high);
      else merged.push({ ...range });
    }
    this.ranges = merged;
  }
  prune(first: number): void {
    if (!Number.isSafeInteger(first) || first < 1) throw new Error("HISTORY_VISIBILITY_REQUIRED");
    this.first = Math.max(this.first, first);
    for (const sequence of this.records.keys()) if (sequence < this.first) this.records.delete(sequence);
    this.ranges = this.ranges.filter(range => range.high > this.first).map(range => ({ ...range, low: Math.max(range.low, this.first) }));
  }
  reconcile(messages: { messageId: string; lifecycle: string }[]): void {
    const removed = new Map(messages.filter(message => message.lifecycle !== "ACTIVE").map(message => [message.messageId, message.lifecycle]));
    for (const [sequence, record] of this.records) {
      const target = record.kind === "EDIT" ? record.targetMessageId : record.payload?.t === "reaction" ? record.payload.target : null;
      const lifecycle = (record.messageId ? removed.get(record.messageId) : null) ?? (target ? removed.get(target) : null);
      if (!lifecycle) continue;
      this.records.set(sequence, { sequence, messageId: record.messageId, senderPrincipalId: record.senderPrincipalId, senderDeviceId: record.senderDeviceId, kind: record.kind, createdAt: record.createdAt, targetMessageId: record.targetMessageId, replyToMessageId: record.replyToMessageId, status: "REMOVED", lifecycle,
        ...(record.privateControl === "REACTION" || record.payload?.t === "reaction" ? { privateControl: "REACTION" as const } : {}) });
    }
  }
}
