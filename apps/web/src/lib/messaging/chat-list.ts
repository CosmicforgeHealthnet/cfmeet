/**
 * F1: the member's own archive and pin of a chat (WhatsApp). Per person, never shown to
 * anyone else; the server returns both in each conversation's inbox summary and refuses a
 * fourth pin (409 PIN_LIMIT_REACHED). Pinning un-archives a chat, archiving un-pins it.
 */

/** Most chats one person can pin (the server's limit too). */
export const MAX_PINNED_CHATS = 3;

/** The chat-list fields of a conversation's inbox summary (absent: an older server). */
export type InboxFlags = { archived: boolean; pinned: boolean; pinnedAt: string | null };

export function inboxFlags(inbox: { archived?: boolean; pinned?: boolean; pinnedAt?: string | null } | undefined, previous?: InboxFlags): InboxFlags {
  if (!inbox || inbox.archived === undefined) return previous ?? { archived: false, pinned: false, pinnedAt: null };
  return { archived: !!inbox.archived, pinned: !!inbox.pinned, pinnedAt: inbox.pinnedAt ?? null };
}

type Listed = InboxFlags & { lastActivityAt: string };

/**
 * The chats one list shows: the main list hides archived chats and puts pinned chats first
 * (the most recently pinned first); the Archived list shows only archived chats. Otherwise
 * the order is kept (latest activity first).
 */
export function arrangeChats<T extends Listed>(chats: T[], archived: boolean): T[] {
  const shown = chats.filter((c) => c.archived === archived);
  if (archived) return shown;
  const pinned = shown.filter((c) => c.pinned).sort((a, b) => (b.pinnedAt ?? "").localeCompare(a.pinnedAt ?? ""));
  return [...pinned, ...shown.filter((c) => !c.pinned)];
}

/** How many chats are pinned (the main list's pin limit). */
export const pinnedCount = (chats: Listed[]) => chats.filter((c) => c.pinned && !c.archived).length;
