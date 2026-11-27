/**
 * The chat list (WhatsApp): the member's own archive, pin and favourite of each chat (per
 * person, never shown to anyone else; the server returns them in each conversation's inbox
 * summary and refuses a fourth pin, 409 PIN_LIMIT_REACHED), the member's own chat lists
 * ("Family", "Work"; Messaging `/v1/messaging/chat-lists`), the filter chips, and what a row
 * previews. Pinning un-archives a chat, archiving un-pins it; favourites are independent.
 *
 * Pure (no imports): node tests run this file directly (tests/messaging-chat-controls.test.mjs).
 */

/** Most chats one person can pin (the server's limit too). */
export const MAX_PINNED_CHATS = 3;
/** Most chat lists one person can have, and chats per list (the server's limits too). */
export const MAX_CHAT_LISTS = 20;
export const MAX_CHATS_PER_LIST = 500;
export const MAX_CHAT_LIST_NAME = 40;

/** The chat-list fields of a conversation's inbox summary (absent: an older server). */
export type InboxFlags = { archived: boolean; pinned: boolean; pinnedAt: string | null; favourite: boolean };

export function inboxFlags(
  inbox: { archived?: boolean; pinned?: boolean; pinnedAt?: string | null; favourite?: boolean } | undefined,
  previous?: InboxFlags,
): InboxFlags {
  if (!inbox || inbox.archived === undefined) return previous ?? { archived: false, pinned: false, pinnedAt: null, favourite: false };
  return {
    archived: !!inbox.archived,
    pinned: !!inbox.pinned,
    pinnedAt: inbox.pinnedAt ?? null,
    favourite: inbox.favourite === undefined ? (previous?.favourite ?? false) : !!inbox.favourite,
  };
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

// ------------------------------------------------------------------ chat lists and filters

/** One of the member's own chat lists (server-stored, the same on every device). */
export type ChatListView = { listId: string; name: string; position: number; conversationIds: string[] };

/**
 * The chip a chat list is filtered by. A later filter (Communities) adds its own kind here,
 * to `matchesChatFilter` and to `filterChips` — nothing else has to change.
 */
export type ChatFilter =
  | { kind: "all" }
  | { kind: "unread" }
  | { kind: "favourites" }
  | { kind: "groups" }
  | { kind: "private" }
  | { kind: "status" }
  | { kind: "list"; listId: string };

export const ALL_CHATS: ChatFilter = { kind: "all" };

export const filterKey = (f: ChatFilter) => (f.kind === "list" ? `list:${f.listId}` : f.kind);

type Filterable = { id: string; unreadCount: number; favourite: boolean; contextClass: string; securityProfile: string };

/** Whether a chat belongs to the chip's view (the open chat never counts as unread). */
export function matchesChatFilter(c: Filterable, filter: ChatFilter, ctx: { selected: string | null; lists: readonly ChatListView[] }): boolean {
  switch (filter.kind) {
    case "all":
    case "status":
      return true;
    case "unread":
      return c.unreadCount > 0 && c.id !== ctx.selected;
    case "favourites":
      return c.favourite;
    case "groups":
      return c.contextClass === "GROUP";
    case "private":
      return c.securityProfile === "PRIVATE_E2EE";
    case "list":
      return !!ctx.lists.find((l) => l.listId === filter.listId)?.conversationIds.includes(c.id);
  }
}

export type FilterChip = { key: string; label: string; filter: ChatFilter; count?: number };

/** WhatsApp's chips: All, Unread (with its count), Favourites, Groups, (Private), then each list. */
export function filterChips(args: { unread: number; hasPrivate: boolean; lists: readonly ChatListView[] }): FilterChip[] {
  const chips: FilterChip[] = [
    { key: "all", label: "All", filter: ALL_CHATS },
    { key: "unread", label: "Unread", filter: { kind: "unread" }, count: args.unread || undefined },
    { key: "favourites", label: "Favourites", filter: { kind: "favourites" } },
    { key: "groups", label: "Groups", filter: { kind: "groups" } },
  ];
  // "Private" only while the account still has older encrypted chats.
  if (args.hasPrivate) chips.push({ key: "private", label: "Private", filter: { kind: "private" } });
  for (const l of sortLists(args.lists)) chips.push({ key: `list:${l.listId}`, label: l.name, filter: { kind: "list", listId: l.listId } });
  return chips;
}

export const sortLists = (lists: readonly ChatListView[]) => [...lists].sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));

/** A filter whose list was deleted (here or on another device) falls back to All. */
export function liveFilter(filter: ChatFilter, lists: readonly ChatListView[]): ChatFilter {
  return filter.kind === "list" && !lists.some((l) => l.listId === filter.listId) ? ALL_CHATS : filter;
}

/** A trimmed list name of 1-40 characters without control characters (null: not valid). */
export function validListName(name: string): string | null {
  const n = name.trim();
  return n && [...n].length <= MAX_CHAT_LIST_NAME && !/[\u0000-\u001f\u007f]/.test(n) ? n : null;
}

/** The lists after one moved to `position` (dense positions, the others keep their order). */
export function moveList(lists: readonly ChatListView[], listId: string, position: number): ChatListView[] {
  const ordered = sortLists(lists);
  const moving = ordered.find((l) => l.listId === listId);
  if (!moving) return ordered;
  const rest = ordered.filter((l) => l.listId !== listId);
  rest.splice(Math.max(0, Math.min(position, rest.length)), 0, moving);
  return rest.map((l, i) => ({ ...l, position: i }));
}

// ------------------------------------------------------------------ row previews

export type PreviewKind = "poll" | "photo" | "video" | "voice" | "audio" | "document" | "file";

/**
 * The icon of a chat's last message from the server summary (MANAGED: presentation and the
 * first attachment's kind; older servers: a file name in the preview text, else a paperclip).
 */
export function summaryPreviewKind(s: { presentation?: string | null; attachmentKind?: string | null; attachmentCount: number; previewText: string | null; lifecycle: string } | null | undefined): PreviewKind | null {
  if (!s || s.lifecycle !== "ACTIVE") return null;
  if (s.presentation === "POLL") return "poll";
  if (!s.attachmentCount) return null;
  switch (s.attachmentKind) {
    case "IMAGE": return "photo";
    case "VIDEO": return "video";
    case "VOICE_NOTE": return "voice";
    case "AUDIO": return "audio";
    case "DOCUMENT": return "document";
    case undefined:
    case null: break;
    default: return "file";
  }
  const name = s.previewText ?? "";
  if (/^voice note /i.test(name)) return "voice";
  if (/\.(jpe?g|png|gif|webp|heic|avif)$/i.test(name)) return "photo";
  if (/\.(mp4|mov|webm|mkv)$/i.test(name)) return "video";
  if (/\.(m4a|ogg|mp3|aac|wav|amr|opus)$/i.test(name)) return "audio";
  if (/\.(pdf|docx?|xlsx?|pptx?|txt|csv|zip)$/i.test(name)) return "document";
  return "file";
}

/** What a chat row says for its unsent draft ("Draft: …"); null without one. */
export function draftPreview(draft: string | null | undefined): string | null {
  const line = (draft ?? "").replace(/\s+/g, " ").trim();
  return line ? (line.length > 80 ? `${line.slice(0, 80)}…` : line) : null;
}
