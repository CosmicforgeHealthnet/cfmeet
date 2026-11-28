"use client";

/**
 * Starred (WhatsApp, brief §19; ADR 0074): everything you starred in one place — chat and
 * group messages (your saved messages in the Communications Hub, the same on every device)
 * and, where the channel store is deployed, channel posts. Search, filter chips, jump to the
 * original, and remove. Nothing is copied: each item is checked again on every read, and one
 * that was deleted, expired, hidden or is no longer yours to see is never shown. A private
 * chat's message shows its text only on a device that decrypted it.
 */
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useInfiniteQuery, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Megaphone, MessageCircle, Search, Star, StarOff, Users, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useSharedMessagingEngine } from "@/components/messaging/MessagingEngineProvider";
import type { EngineSnapshot, MessagingEngine } from "@/lib/messaging/engine";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { arrangeStarred, channelItems, messageItems, starredChips, type ChannelStar, type StarredFilter, type StarredItem } from "@/lib/starred";

/** The account's saved (starred) messages: shared with the in-chat Star action. */
export const SAVED_KEY = ["saved-messages"] as const;
const CHANNEL_STARS_KEY = ["starred", "channel-posts"] as const;
/** At most this many Hub pages (50 each) are read: the Hub keeps up to 1000 stars. */
const MAX_PAGES = 20;

/**
 * A channel-post star store and its unstar (ADR 0072 channels): passed in once that store is
 * deployed; without it the Channels chip does not appear.
 */
export type ChannelStars = { load: () => Promise<ChannelStar[]>; unstar: (channelId: string, postId: string) => Promise<unknown> };

const EMPTY: EngineSnapshot["conversations"] = [];

function useConversations(engine: MessagingEngine | null) {
  const subscribe = useMemo(() => (engine ? engine.subscribe : () => () => {}), [engine]);
  const snapshot = useMemo(() => (engine ? engine.getSnapshot : () => null), [engine]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

const SOURCE_ICON = { chat: MessageCircle, group: Users, channel: Megaphone } as const;

export default function StarredContent({ channelStars }: { channelStars?: ChannelStars }) {
  const engine = useSharedMessagingEngine();
  const s = useConversations(engine);
  const client = useQueryClient();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<StarredFilter>("all");
  const [removing, setRemoving] = useState<string | null>(null);

  const saved = useInfiniteQuery({
    queryKey: [...SAVED_KEY, "all"],
    queryFn: ({ pageParam }) => hub.savedMessages(pageParam),
    initialPageParam: null as string | null,
    getNextPageParam: (last, pages) => (pages.length < MAX_PAGES ? last.nextCursor : null),
    retry: 1,
  });
  // Read every page (stars are few; search and filters then cover all of them).
  useEffect(() => {
    if (saved.hasNextPage && !saved.isFetchingNextPage) void saved.fetchNextPage();
  }, [saved]);
  const channels = useQuery({ queryKey: CHANNEL_STARS_KEY, queryFn: () => channelStars!.load(), enabled: !!channelStars, retry: 1 });

  const entries = useMemo(() => saved.data?.pages.flatMap((p) => p.data) ?? [], [saved.data]);
  const conversations = s?.conversations ?? EMPTY;
  const chat = (id: string) => {
    const c = conversations.find((x) => x.id === id);
    return c ? { title: c.title, group: c.contextClass === "GROUP" } : null;
  };
  // Text this device can read: loaded messages, else the member read of a synced chat's message.
  const wanted = entries.filter((e) => e.available && chat(e.conversationId));
  const texts = useQueries({
    queries: wanted.map((e) => ({
      queryKey: ["starred", "text", e.conversationId, e.messageId],
      queryFn: () => engine!.starredText(e.conversationId, e.messageId),
      enabled: !!engine,
      staleTime: 5 * 60_000,
      retry: false,
    })),
  });
  const textOf = new Map(wanted.map((e, i) => [`${e.conversationId}|${e.messageId}`, texts[i]?.data ?? null]));
  const items: StarredItem[] = [
    ...messageItems(entries, chat, (c, m) => textOf.get(`${c}|${m}`) ?? null),
    ...(channelStars ? channelItems(channels.data ?? []) : []),
  ];
  const chips = starredChips(items, !!channelStars);
  const shown = arrangeStarred(items, chips.some((c) => c.key === filter) ? filter : "all", query);
  const loading = saved.isPending || (!!channelStars && channels.isPending);

  const remove = async (item: StarredItem) => {
    setRemoving(item.key);
    try {
      if (item.removeRef.kind === "saved") {
        await hub.unsaveMessage(item.removeRef.savedId);
        await client.invalidateQueries({ queryKey: SAVED_KEY });
      } else if (channelStars) {
        await channelStars.unstar(item.removeRef.channelId, item.removeRef.postId);
        await client.invalidateQueries({ queryKey: CHANNEL_STARS_KEY });
      }
      toast("Removed from Starred");
    } catch {
      toast("Couldn't remove it. Try again.", "error");
    } finally {
      setRemoving(null);
    }
  };

  return (
    <div className="mx-auto max-w-2xl px-4 pb-28 pt-6 sm:px-6 lg:py-8" data-testid="starred-page">
      <header className="flex items-center gap-2">
        <Link href="/dashboard/messages" aria-label="Back to Chats" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <h1 className="min-w-0 flex-1 truncate text-2xl font-bold tracking-tight">Starred</h1>
      </header>
      <label className="mt-4 flex h-11 items-center gap-2 rounded-full bg-muted px-4">
        <Search className="size-4 text-muted-foreground" />
        <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search starred" aria-label="Search starred" data-testid="starred-search"
          className="min-w-0 flex-1 bg-transparent text-sm focus:outline-none" />
        {query ? <button type="button" aria-label="Clear search" onClick={() => setQuery("")} className="text-muted-foreground"><X className="size-4" /></button> : null}
      </label>
      {chips.length > 1 ? (
        <div className="no-scrollbar -mx-1 mt-3 flex gap-2 overflow-x-auto px-1" role="tablist" aria-label="Filter starred">
          {chips.map((c) => (
            <button key={c.key} type="button" role="tab" aria-selected={filter === c.key} data-testid={`starred-filter-${c.key}`} onClick={() => setFilter(c.key)}
              className={cn("h-8 shrink-0 rounded-full px-3.5 text-sm font-medium", filter === c.key ? "bg-primary/15 text-primary" : "bg-muted/60 text-muted-foreground hover:bg-muted hover:text-foreground")}>
              {c.label}<span className="ml-1 tabular-nums opacity-70">{c.count}</span>
            </button>
          ))}
        </div>
      ) : null}

      {loading ? <div className="mt-4 space-y-2">{[0, 1, 2].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</div> : null}
      {saved.isError ? (
        <p role="alert" className="mt-4 text-sm text-destructive">Starred messages couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void saved.refetch()}>Try again</button></p>
      ) : null}
      {!loading && !saved.isError && items.length === 0 ? (
        <div className="mt-10 flex flex-col items-center px-8 text-center" data-testid="starred-empty">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-amber-400/15 text-amber-500"><Star className="size-6" /></span>
          <p className="mt-4 font-semibold">Nothing starred yet</p>
          <p className="mt-1 text-sm text-muted-foreground">Open a message&apos;s menu and choose Star to keep it here, on every device you sign in on.</p>
        </div>
      ) : null}
      {!loading && items.length > 0 && shown.length === 0 ? <p className="mt-6 text-center text-sm text-muted-foreground">Nothing starred matches “{query}”.</p> : null}

      <ul className="mt-3 divide-y divide-border rounded-2xl bg-card ring-1 ring-border empty:hidden" data-testid="starred-list">
        {shown.map((item) => {
          const Icon = SOURCE_ICON[item.source];
          return (
            <li key={item.key} className="flex items-start gap-3 px-3 py-3" data-testid="starred-item" data-source={item.source}>
              <span className="mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"><Icon className="size-4" aria-hidden /></span>
              <Link href={item.href} className="min-w-0 flex-1" data-testid="starred-open">
                <span className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
                  <span className="truncate font-semibold text-foreground">{item.where}</span>
                  <time className="shrink-0">{new Date(item.starredAt).toLocaleDateString([], { day: "numeric", month: "short" })}</time>
                </span>
                <span className="mt-1 line-clamp-3 block text-sm">{item.text ?? <span className="text-muted-foreground">Open the chat to see this message</span>}</span>
              </Link>
              <button type="button" aria-label="Remove from Starred" title="Remove from Starred" disabled={removing === item.key} onClick={() => void remove(item)} data-testid="starred-remove"
                className="flex size-9 shrink-0 items-center justify-center rounded-full text-amber-500 hover:bg-muted disabled:opacity-50"><StarOff className="size-4" /></button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
