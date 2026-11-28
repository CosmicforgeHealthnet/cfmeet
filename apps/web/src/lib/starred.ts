/**
 * Unified starred content (WhatsApp "Starred", brief §19): starred personal, group and channel
 * messages / posts in one list with search, filters, jump to the original and remove.
 *
 * Where each kind lives (no copy of anything, so starring never widens access):
 * - Chat and group messages (communities reuse groups): the account's saved messages in the
 *   Communications Hub (`/saved-messages`, the same on every device). The Hub stores only
 *   which message it is and checks every item with Messaging on each read; an item that was
 *   deleted, expired, hidden, cleared, or sits in a chat the person left comes back
 *   unavailable and is NOT shown here ({@link visibleSaved}). Text is what this device
 *   already reads (a private chat's message is decrypted only on the device).
 * - Channel posts: the channel's own stars in Messaging, once that store is deployed (its
 *   items arrive through {@link channelItems}).
 *
 * Pure (no imports): node tests run this file directly (tests/starred.test.mjs).
 */

export type StarredSource = "chat" | "group" | "channel";
export type StarredFilter = "all" | StarredSource;

export type StarredItem = {
  /** Stable key across sources. */
  key: string;
  source: StarredSource;
  /** When it was starred (newest first). */
  starredAt: string;
  /** The chat or channel name. */
  where: string;
  /** What it says (null: not readable on this device yet). */
  text: string | null;
  /** Remove handle: the Hub's savedId, or the channel post. */
  removeRef: { kind: "saved"; savedId: string } | { kind: "channel"; channelId: string; postId: string };
  /** Where "jump to the original" goes. */
  href: string;
  conversationId?: string;
  messageId?: string;
};

/** One Hub saved-message entry (communications.service SavedMessage). */
export type SavedEntry = { savedId: string; conversationId: string; messageId: string; savedAt: string; available: boolean; unavailableReason?: string | null };

/** The saved messages that may be shown: anything the Hub reports unavailable never is. */
export const visibleSaved = (entries: readonly SavedEntry[]) => entries.filter((e) => e.available === true);

/** The original message inside its chat (Chats opens it and scrolls to the message). */
export const messageHref = (conversationId: string, messageId: string) =>
  `/dashboard/messages?conversation=${encodeURIComponent(conversationId)}&message=${encodeURIComponent(messageId)}`;

/** The original post inside its channel. */
export const channelPostHref = (channelId: string, postId: string) =>
  `/dashboard/updates/channels/${encodeURIComponent(channelId)}?post=${encodeURIComponent(postId)}`;

/**
 * Starred chat / group messages as list items. `chat(conversationId)` answers what this
 * device knows of the chat (null: it is not in the account's chat list any more, so the item
 * is not shown either); `text` what the message says here (null: not loaded yet).
 */
export function messageItems(
  entries: readonly SavedEntry[],
  chat: (conversationId: string) => { title: string; group: boolean } | null,
  text: (conversationId: string, messageId: string) => string | null,
): StarredItem[] {
  const out: StarredItem[] = [];
  for (const e of visibleSaved(entries)) {
    const c = chat(e.conversationId);
    if (!c) continue;
    out.push({
      key: `saved:${e.savedId}`,
      source: c.group ? "group" : "chat",
      starredAt: e.savedAt,
      where: c.title,
      text: text(e.conversationId, e.messageId),
      removeRef: { kind: "saved", savedId: e.savedId },
      href: messageHref(e.conversationId, e.messageId),
      conversationId: e.conversationId,
      messageId: e.messageId,
    });
  }
  return out;
}

/** One starred channel post as the channel store returns it (only posts the caller may still read). */
export type ChannelStar = { starredAt: string; channel: { channelId: string; name: string }; post: { postId: string; text: string | null; kind?: string } };

const POST_KIND: Record<string, string> = { IMAGE: "Photo", VIDEO: "Video", AUDIO: "Audio", VOICE: "Voice message", DOCUMENT: "Document", GIF: "GIF", STICKER: "Sticker", POLL: "Poll" };

export function channelItems(stars: readonly ChannelStar[]): StarredItem[] {
  return stars.map((s) => ({
    key: `channel:${s.post.postId}`,
    source: "channel" as const,
    starredAt: s.starredAt,
    where: s.channel.name,
    text: s.post.text ?? (s.post.kind ? POST_KIND[s.post.kind] ?? null : null),
    removeRef: { kind: "channel" as const, channelId: s.channel.channelId, postId: s.post.postId },
    href: channelPostHref(s.channel.channelId, s.post.postId),
  }));
}

/** Newest star first, then the filter chip and the search (name or text, case-insensitive). */
export function arrangeStarred(items: readonly StarredItem[], filter: StarredFilter, query: string): StarredItem[] {
  const q = query.trim().toLocaleLowerCase();
  return items
    .filter((i) => filter === "all" || i.source === filter)
    .filter((i) => !q || i.where.toLocaleLowerCase().includes(q) || (i.text ?? "").toLocaleLowerCase().includes(q))
    .sort((a, b) => b.starredAt.localeCompare(a.starredAt) || a.key.localeCompare(b.key));
}

/** The filter chips with their counts (a chip appears only when it has items, All always). */
export function starredChips(items: readonly StarredItem[], channels: boolean): { key: StarredFilter; label: string; count: number }[] {
  const count = (s: StarredSource) => items.filter((i) => i.source === s).length;
  const chips: { key: StarredFilter; label: string; count: number }[] = [{ key: "all", label: "All", count: items.length }];
  if (count("chat")) chips.push({ key: "chat", label: "Chats", count: count("chat") });
  if (count("group")) chips.push({ key: "group", label: "Groups", count: count("group") });
  if (channels && count("channel")) chips.push({ key: "channel", label: "Channels", count: count("channel") });
  return chips;
}
