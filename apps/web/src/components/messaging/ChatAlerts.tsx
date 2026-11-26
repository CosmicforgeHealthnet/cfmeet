"use client";

import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { useRouter } from "next/navigation";
import { useHubCapabilities, useHubInbox } from "@/hooks/v2/useCommunications";
import { communicationsAreas } from "@/lib/communications/comms-core";
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
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const inbox = useHubInbox(!!engine && (areas.contacts || areas.managedMessaging));
  const muted = useMemo(() => new Set((inbox.data?.data ?? []).filter((e) => e.notifications === "MUTED").map((e) => e.conversationId)), [inbox.data]);
  const router = useRouter();
  const seen = useRef<Map<string, number> | null>(null);
  const baseTitle = useRef<string | null>(null);

  // Unread chats in the tab title.
  const unreadChats = (s?.conversations ?? []).filter((c) => c.unreadCount > 0 && !muted.has(c.id) && !c.archived).length;
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
    const next = new Map(s.conversations.map((c) => [c.id, c.lastMessage?.sequence ?? 0]));
    seen.current = next;
    if (!previous || typeof window === "undefined" || !("Notification" in window) || Notification.permission !== "granted") return;
    const open = window.location.pathname.startsWith("/dashboard/messages") ? new URLSearchParams(window.location.search).get("conversation") : null;
    const me = s.identity?.principalId;
    for (const c of s.conversations) {
      const last = c.lastMessage;
      if (!last || last.senderPrincipalId === me || muted.has(c.id) || c.unreadCount === 0) continue;
      if ((previous.get(c.id) ?? 0) >= last.sequence) continue;
      const showing = document.visibilityState === "visible" && (open === c.id || (open === null && window.location.pathname.startsWith("/dashboard/messages")));
      if (showing) continue;
      notify(router, c.id, c.title, c.contextClass === "GROUP" ? s.names[last.senderPrincipalId] ?? null : null,
        last.previewText ?? (last.attachmentCount ? "📎 Attachment" : "New message"));
    }
  }, [s, muted, router]);

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
