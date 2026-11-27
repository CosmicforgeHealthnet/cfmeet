export type LocalHistoryCursor = { conversationId: string; sequence: number };
export type LocalHistoryPage = { records: unknown[]; next: LocalHistoryCursor | null };

/** One local file, with explicit bounds and continuation if this device has more. */
export async function collectDeviceHistory(read: (after: LocalHistoryCursor | null) => Promise<LocalHistoryPage>) {
  const records: unknown[] = [];
  let next: LocalHistoryCursor | null = null;
  let size = 0;
  const seen = new Set<string>();
  for (let page = 0; page < 1000; page++) {
    const result = await read(next);
    const bytes = new TextEncoder().encode(JSON.stringify(result.records)).length;
    if (size + bytes > 50 * 1024 * 1024) return { records, complete: false, next };
    records.push(...result.records); size += bytes;
    next = result.next;
    if (!next) return { records, complete: true, next: null };
    const key = JSON.stringify(next);
    if (seen.has(key)) throw new Error("The local history cursor did not advance");
    seen.add(key);
  }
  return { records, complete: false, next };
}

