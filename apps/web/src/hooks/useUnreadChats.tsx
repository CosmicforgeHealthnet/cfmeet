"use client";

import { useMemo, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { relay } from "@/services/v2/messaging.service";
import { useHubCapabilities, useHubInbox } from "@/hooks/v2/useCommunications";
import { communicationsAreas } from "@/lib/communications/comms-core";
import { useSharedMessagingEngine } from "@/components/messaging/MessagingEngineProvider";
import type { InboxEntry } from "@/types/communications";

type Page = { conversations: { inbox?: { unreadCount?: number } | null }[] };

/** A chat's notification setting as it applies now (an expired "mute for 8 hours" is over). */
export function effectiveNotifications(entry: InboxEntry | undefined, now = Date.now()): "ALL" | "MUTED" | "MENTIONS" {
  if (!entry) return "ALL";
  if (entry.notifications === "MUTED" && entry.mutedUntil && Date.parse(entry.mutedUntil) <= now) return "ALL";
  return entry.notifications;
}

/** Each chat's notification setting, from the Communications Hub inbox (shared cache). */
export function useChatNotificationSettings(): Map<string, InboxEntry> {
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const inbox = useHubInbox(areas.contacts || areas.managedMessaging);
  return useMemo(() => new Map((inbox.data?.data ?? []).map((e) => [e.conversationId, e])), [inbox.data]);
}

const NO_SNAPSHOT = () => null;
const NO_SUBSCRIBE = () => () => {};

/**
 * How many chats have messages you haven't read (the number on the Chats tab and in the tab
 * title, like WhatsApp): from the app-wide messaging engine, live, without muted or archived
 * chats. Outside the signed-in app (no engine) it falls back to the server's list.
 */
export function useUnreadChats() {
  const engine = useSharedMessagingEngine();
  const s = useSyncExternalStore(engine ? engine.subscribe : NO_SUBSCRIBE, engine ? engine.getSnapshot : NO_SNAPSHOT, engine ? engine.getSnapshot : NO_SNAPSHOT);
  const settings = useChatNotificationSettings();
  const fallback = useQuery({
    queryKey: ["messaging", "unread-chats"],
    queryFn: async () => (await relay<Page>("GET", "v1/conversations?limit=100&orderBy=lastActivity")).conversations.filter((c) => (c.inbox?.unreadCount ?? 0) > 0).length,
    refetchInterval: 20_000,
    refetchOnWindowFocus: true,
    retry: false,
    enabled: !engine,
  });
  if (!s) return fallback.data ?? 0;
  return s.conversations.filter((c) => c.unreadCount > 0 && !c.archived && effectiveNotifications(settings.get(c.id)) !== "MUTED").length;
}

export function UnreadBadge({ count, className }: { count: number; className?: string }) {
  if (!count) return null;
  return (
    <span aria-label={`${count} unread chats`} className={`flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-primary px-1 text-[10px] font-bold leading-none text-primary-foreground ${className ?? ""}`}>
      {count > 99 ? "99+" : count}
    </span>
  );
}
