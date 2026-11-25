import { mergeChatLines, PAGE_SIZE, RelayPathError, toChatLines, type ChatLine, type ServerMessage } from "./relay-paths.ts";

/** Follow opaque cursors to completion; never report a truncated conversation list as complete. */
export async function collectConversationPages<T>(
  fetchPage: (cursor: string | null) => Promise<{ conversations: T[]; nextCursor: string | null }>
): Promise<T[]> {
  const out: T[] = [];
  const seen = new Set<string>();
  let cursor: string | null = null;
  do {
    const page = await fetchPage(cursor);
    out.push(...page.conversations);
    cursor = page.nextCursor;
    if (cursor) {
      if (seen.has(cursor)) throw new RelayPathError("Conversation pagination did not advance. Please retry.");
      seen.add(cursor);
    }
  } while (cursor);
  return out;
}

/** Cursor and its materialized history must be retained or discarded together. */
export interface ConversationHistory {
  lines: ChatLine[];
  afterSequence: number;
}

/** A bounded polling batch resumes next time from the same cached history, including non-display events. */
export async function readConversationHistory(
  previous: ConversationHistory | undefined,
  me: string | null,
  fetchPage: (afterSequence: number) => Promise<{ messages: ServerMessage[] }>
): Promise<ConversationHistory> {
  let lines = previous?.lines ?? [];
  let afterSequence = previous?.afterSequence ?? 0;
  for (let i = 0; i < 20; i++) {
    const page = await fetchPage(afterSequence);
    const next = page.messages.reduce((m, x) => Math.max(m, x.sequence), afterSequence);
    if (page.messages.length && next <= afterSequence) {
      throw new RelayPathError("Message pagination did not advance. Please retry.");
    }
    lines = mergeChatLines(lines, toChatLines(page.messages, me));
    afterSequence = next;
    if (page.messages.length < PAGE_SIZE) break;
  }
  return { lines, afterSequence };
}
