"use client";

import { useEffect, useRef, useSyncExternalStore } from "react";
import { useRouter } from "next/navigation";
import { effectiveNotifications, useChatNotificationSettings, useUnreadChats } from "@/hooks/useUnreadChats";
import { useSharedMessagingEngine } from "./MessagingEngineProvider";

const NO_SNAPSHOT = () => null;
const NO_SUBSCRIBE = () => () => {};

/**
 * WhatsApp Web's attention signals, on every page of the signed-in app:
 * - the tab title counts unread chats ("(3) CF Meet");
 * - a desktop notification (sender and text) for a new message while the tab is hidden
 *   or that chat isn't open, once the user turned them on. Muted chats stay silent.
 */
export function ChatAlerts() {
  const engine = useSharedMessagingEngine();
  const s = useSyncExternalStore(
    engine ? engine.subscribe : NO_SUBSCRIBE,
    engine ? engine.getSnapshot : NO_SNAPSHOT,
    engine ? engine.getSnapshot : NO_SNAPSHOT,
  );
  const settings = useChatNotificationSettings();
  const router = useRouter();
  const seen = useRef<Map<string, { sequence: number; mentions: number }> | null>(null);
  const baseTitle = useRef<string | null>(null);

  // Unread chats in the tab title: the same count as the Chats badge.
  const unreadChats = useUnreadChats();
  useEffect(() => {
    if (typeof document === "undefined") return;
    if (baseTitle.current === null) baseTitle.current = document.title.replace(/^\(\d+\) /, "");
    const base = document.title.replace(/^\(\d+\) /, "") || baseTitle.current;
    document.title = unreadChats ? `(${unreadChats}) ${base}` : base;
  }, [unreadChats]);

  // Desktop notifications for messages that just arrived.
  useEffect(() => {
    if (!s) return;
    const previous = seen.current;
    const next = new Map(s.conversations.map((c) => [c.id, { sequence: c.lastMessage?.sequence ?? 0, mentions: c.mentionCount }]));
    seen.current = next;
    if (!previous || typeof window === "undefined" || !("Notification" in window) || Notification.permission !== "granted") return;
    const open = window.location.pathname.startsWith("/dashboard/messages") ? new URLSearchParams(window.location.search).get("conversation") : null;
    const me = s.identity?.principalId;
    for (const c of s.conversations) {
      const last = c.lastMessage;
      if (!last || last.senderPrincipalId === me || last.lifecycle !== "ACTIVE") continue;
      const before = previous.get(c.id);
      if ((before?.sequence ?? 0) >= last.sequence) continue;
      // The chat's setting as it applies now: muted (until it expires), only @mentions, or all.
      const setting = effectiveNotifications(settings.get(c.id));
      if (setting === "MUTED" || (setting === "MENTIONS" && c.mentionCount <= (before?.mentions ?? 0))) continue;
      const showing = document.visibilityState === "visible" && (open === c.id || (open === null && window.location.pathname.startsWith("/dashboard/messages")));
      if (showing) continue;
      notify(router, c.id, c.title, c.contextClass === "GROUP" ? s.names[last.senderPrincipalId] ?? null : null,
        last.previewText ?? (last.attachmentCount ? "📎 Attachment" : "New message"));
    }
  }, [s, settings, router]);

  return null;
}

function notify(router: ReturnType<typeof useRouter>, conversationId: string, title: string, sender: string | null, text: string) {
  try {
    const n = new Notification(title, { body: sender ? `${sender.split(" ")[0]}: ${text}` : text, tag: `chat:${conversationId}`, icon: "/favicon.ico" });
    n.onclick = () => {
      window.focus();
      router.push(`/dashboard/messages?conversation=${encodeURIComponent(conversationId)}`);
      n.close();
    };
  } catch { /* notifications unavailable */ }
}
