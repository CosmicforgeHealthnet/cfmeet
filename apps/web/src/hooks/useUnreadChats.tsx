"use client";

import { useQuery } from "@tanstack/react-query";
import { relay } from "@/services/v2/messaging.service";

type Page = { conversations: { inbox?: { unreadCount?: number } | null }[] };

/** How many chats have messages you haven't read (the number on the Chats tab, like WhatsApp). */
export function useUnreadChats() {
  const q = useQuery({
    queryKey: ["messaging", "unread-chats"],
    queryFn: async () => (await relay<Page>("GET", "v1/conversations?limit=100&orderBy=lastActivity")).conversations.filter((c) => (c.inbox?.unreadCount ?? 0) > 0).length,
    refetchInterval: 20_000,
    refetchOnWindowFocus: true,
    retry: false,
  });
  return q.data ?? 0;
}

export function UnreadBadge({ count, className }: { count: number; className?: string }) {
  if (!count) return null;
  return (
    <span aria-label={`${count} unread chats`} className={`flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-primary px-1 text-[10px] font-bold leading-none text-primary-foreground ${className ?? ""}`}>
      {count > 99 ? "99+" : count}
    </span>
  );
}
