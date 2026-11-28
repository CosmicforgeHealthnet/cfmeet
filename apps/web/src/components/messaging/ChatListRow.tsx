"use client";

/**
 * One row of the chat list (WhatsApp): photo, name, time, the last message's preview with
 * its kind (photo, video, voice note, document, poll), your own message's ticks, typing,
 * an unsent draft ("Draft: …", kept in this browser only), and the unread / @ badges with
 * pinned, muted, favourite and archived marks. ADR 0073: a newer call (voice / video,
 * missed in red) or the other person's reaction to your status replaces the preview; a
 * status reply or share shows the status icon with its context; a status that mentions you
 * adds a badge; a group without messages says who created it. Memoised: a row re-renders
 * only when its own chat changes, so a long list stays smooth while messages arrive.
 */
import { memo, useEffect, useState } from "react";
import Image from "next/image";
import { Archive, BarChart3, BellOff, Camera, Check, CheckCheck, CircleDashed, Clock3, FileText, Headphones, Heart, Lock, Mic, Paperclip, Phone, PhoneMissed, Pin, Video } from "lucide-react";
import { cn } from "@/lib/utils";
import type { ConversationView, MessageView, MessagingEngine } from "@/lib/messaging/engine";
import { attachmentKind, chatPreview } from "@/lib/messaging/e2ee-membership";
import { draftPreview, summaryPreviewKind, type PreviewKind } from "@/lib/messaging/chat-list";
import { rowOverlay, statusMessage, statusMessagePreview, type CallPreview, type StatusReaction } from "@/lib/messaging/chat-indicators";
import type { StatusRing } from "@/services/v2/social.service";
import { StatusRingFrame } from "@/components/updates/StatusRing";

const AVATAR_COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
export const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

/** Initials on a colour from the name; a group's photo instead when it has one. */
export function Avatar({ name, seed, size = "size-11", className, photo }: { name: string; seed?: string; size?: string; className?: string; photo?: string | null }) {
  if (photo) {
    return <Image src={photo} alt="" width={96} height={96} unoptimized aria-hidden className={cn("shrink-0 rounded-full object-cover", size, className)} />;
  }
  return (
    <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size, className)} style={{ backgroundColor: colorFor(name.trim().toLowerCase() || seed || "?") }}>
      {initials(name)}
    </span>
  );
}

/** A synced group's photo (`avatarRef` attachment), loaded once per photo; null meanwhile. */
export function useGroupPhoto(engine: MessagingEngine, conversationId: string, avatarRef: string | null): string | null {
  const key = avatarRef?.startsWith("attachment:") ? `${conversationId}|${avatarRef}` : "";
  const [loaded, setLoaded] = useState<{ key: string; url: string | null }>({ key: "", url: null });
  useEffect(() => {
    if (!key) return;
    let live = true;
    void engine.avatarUrl(conversationId, avatarRef).then((url) => {
      if (live) setLoaded({ key, url });
    });
    return () => {
      live = false;
    };
  }, [engine, conversationId, avatarRef, key]);
  return key && loaded.key === key ? loaded.url : null;
}

export function listTime(iso: string) {
  const d = new Date(iso);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const yesterday = new Date(); yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  if (now.getTime() - d.getTime() < 6 * 86400000) return d.toLocaleDateString([], { weekday: "short" });
  return d.toLocaleDateString([], { day: "numeric", month: "short" });
}

/** WhatsApp ticks: one = sent, two = delivered, two coloured = read. */
export function Ticks({ receipt, list }: { receipt: "sent" | "delivered" | "read"; list?: boolean }) {
  if (receipt === "sent") return <Check className={cn("size-4 shrink-0", !list && "opacity-70")} aria-label="Sent" />;
  if (receipt === "delivered") return <CheckCheck className={cn("size-4 shrink-0", !list && "opacity-70")} aria-label="Delivered" />;
  // Read: on your own blue bubble a bright aqua with a heavier stroke (blue on blue would vanish).
  return <CheckCheck strokeWidth={2.75} className={cn("size-4 shrink-0", list ? "text-sky-500" : "text-[#7CFFF4] drop-shadow-[0_0_1px_rgba(0,0,0,0.35)]")} aria-label="Read" />;
}

/** One tick state for a message from every other member's state (read only when all read). */
export function summaryReceipt(rows: { state: "read" | "delivered" | "sent" }[]): "read" | "delivered" | "sent" {
  if (!rows.length) return "sent";
  return rows.every((r) => r.state === "read") ? "read" : rows.every((r) => r.state !== "sent") ? "delivered" : "sent";
}

/** The chat list's icon for a message kind (WhatsApp: camera for a photo, mic for a voice message, chart for a poll). */
export const FILE_ICONS = { photo: Camera, video: Video, voice: Mic, audio: Headphones, document: FileText, file: Paperclip, poll: BarChart3 } as const;

export type ChatRowProps = {
  engine: MessagingEngine;
  c: ConversationView;
  /** The newest loaded message of this chat (undefined: not loaded on this device yet). */
  latest: MessageView | undefined;
  loaded: boolean;
  me: string | null;
  /** First names of the latest sender (groups) and of who's typing. */
  senderName: string | null;
  typingName: string | null;
  active: boolean;
  muted: boolean;
  draft: string;
  deviceRegisteredAt: string | null;
  onSelect: (id: string) => void;
  /** A DIRECT chat's other person, when they have a status the caller may see (ADR 0069 rings). */
  statusRing?: StatusRing | null;
  /** Tapping the ringed avatar opens their status; tapping the row still opens the chat. */
  onOpenStatus?: (principalId: string, name: string) => void;
  /** The chat's latest call (Hub timeline card), shown when newer than its last message. */
  call?: CallPreview | null;
  /** A DIRECT chat's other person reacted to one of my live statuses (newer: shown instead). */
  statusReaction?: StatusReaction | null;
  /** A live status of theirs mentions me and I haven't seen it. */
  statusMention?: boolean;
  /** What a group says before its first message ("You created this group"). */
  groupPreview?: string | null;
};

function previewKindOf(latest: MessageView | undefined, c: ConversationView): PreviewKind | null {
  if (latest) {
    if (latest.rich?.kind === "poll") return "poll";
    return latest.file ? attachmentKind(latest.file) : null;
  }
  return c.securityProfile === "PRIVATE_E2EE" ? null : summaryPreviewKind(c.lastMessage);
}

export const ChatListRow = memo(function ChatListRow({ engine, c, latest, loaded, me, senderName, typingName, active, muted, draft, deviceRegisteredAt, onSelect, statusRing, onOpenStatus, call, statusReaction, statusMention, groupPreview }: ChatRowProps) {
  const summary = c.lastMessage;
  const photo = useGroupPhoto(engine, c.id, c.avatarRef);
  // A private message this browser can still receive reads "Waiting for this message";
  // one sent before it joined reads so. The bar only shows while a readable chat loads.
  const preview = chatPreview({ e2ee: c.securityProfile === "PRIVATE_E2EE", groupReady: c.groupReady, loaded, latest, summary, joinedAt: deviceRegisteredAt });
  const at = latest?.createdAt ?? summary?.createdAt ?? null;
  const kind = previewKindOf(latest, c);
  const KindIcon = kind ? FILE_ICONS[kind] : null;
  const mine = latest ? latest.mine : !!summary && summary.senderPrincipalId === me;
  const unread = active ? 0 : c.unreadCount;
  const draftText = active ? null : draftPreview(draft);
  const ringed = !!statusRing?.hasActiveStatus && !!onOpenStatus;
  const overlay = rowOverlay({ lastMessageAt: at, call: call ?? null, reaction: c.contextClass === "DIRECT" ? statusReaction ?? null : null });
  // The server-written text of a status reply / share (what this device already shows).
  const status = statusMessage(latest ? latest.text : c.securityProfile === "PRIVATE_E2EE" ? null : summary?.lifecycle === "ACTIVE" ? summary.previewText : null);
  const statusLine = status ? statusMessagePreview(status, mine) : null;
  const shownAt = overlay?.kind === "call" ? overlay.call.at : overlay?.kind === "status-reaction" ? overlay.reaction.at : at;
  return (
    <li className="relative">
      <button
        type="button"
        data-testid="conversation-item"
        data-profile={c.securityProfile}
        data-unread={unread || undefined}
        aria-current={active ? "true" : undefined}
        onClick={() => onSelect(c.id)}
        className={cn("flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors", active ? "bg-primary/10" : "hover:bg-muted/70")}
      >
        <span className="relative">
          <StatusRingFrame ring={ringed ? statusRing : null}><Avatar name={c.title} seed={c.id} size={ringed ? "size-[42px]" : "size-12"} photo={photo} /></StatusRingFrame>
          {c.securityProfile === "PRIVATE_E2EE" ? <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-background"><Lock className="size-3 text-emerald-600" aria-label="End-to-end encrypted" /></span> : null}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-baseline justify-between gap-2">
            <span className={cn("flex min-w-0 items-center gap-1.5 text-[15px]", unread ? "font-bold" : "font-semibold")}>
              <span className="truncate">{c.title}</span>
              {c.favourite ? <Heart className="size-3.5 shrink-0 fill-rose-500 text-rose-500" aria-label="Favourite" /> : null}
            </span>
            {shownAt ? <time className={cn("shrink-0 text-xs", unread ? "font-semibold text-primary" : "text-muted-foreground")}>{listTime(shownAt)}</time> : null}
          </span>
          <span className="mt-0.5 flex items-center justify-between gap-2">
            <span className={cn("flex min-w-0 items-center gap-1 text-sm", unread ? "font-medium text-foreground" : "text-muted-foreground")}>
              {typingName !== null ? <span className="truncate font-medium text-primary">{c.contextClass === "GROUP" ? `${typingName || "Someone"} is typing…` : "typing…"}</span>
                : draftText ? <span className="truncate" data-testid="draft-preview"><span className="font-medium text-emerald-600 dark:text-emerald-400">Draft: </span>{draftText}</span>
                : overlay?.kind === "call" ? <span className={cn("flex min-w-0 items-center gap-1", overlay.call.missed && "text-destructive")} data-testid="preview-call" data-missed={overlay.call.missed || undefined}>
                  {overlay.call.missed ? <PhoneMissed className="size-4 shrink-0" aria-hidden /> : overlay.call.video ? <Video className="size-4 shrink-0" aria-hidden /> : <Phone className="size-4 shrink-0" aria-hidden />}
                  <span className="truncate">{overlay.call.label}</span>
                </span>
                : overlay?.kind === "status-reaction" ? <span className="flex min-w-0 items-center gap-1" data-testid="preview-status-reaction">
                  <CircleDashed className="size-4 shrink-0 text-primary" aria-hidden /><span className="truncate">Reacted {overlay.reaction.emoji} to your status</span>
                </span>
                : statusLine ? <span className="flex min-w-0 items-center gap-1" data-testid="preview-status-message">
                  <CircleDashed className="size-4 shrink-0 text-primary" aria-label="Status" />
                  <span className="truncate">{status?.kind === "status-reply" ? <><span className="font-medium">{statusLine.context}: </span>{statusLine.text}</> : statusLine.text}</span>
                </span>
                : groupPreview ? <span className="truncate" data-testid="preview-group-activity">{groupPreview}</span>
                : <>
                  {mine && latest && (latest.state === "queued" || latest.state === "sending" || latest.state === "pending-setup") ? <Clock3 className="size-3.5 shrink-0" aria-label="Sending" /> : null}
                  {mine && latest && (latest.state === "sent" || latest.state === "delivered") ? <Ticks receipt={latest.seenBy?.length ? "read" : latest.receipt ?? "sent"} list /> : null}
                  {mine && !latest && summary && summary.sequence > 0 && summary.lifecycle === "ACTIVE" ? <Ticks receipt={summaryReceipt(engine.messageInfo(c.id, summary.sequence))} list /> : null}
                  {latest?.state === "failed" ? <span className="shrink-0 text-destructive">Not sent ·</span> : null}
                  {preview === null
                    ? <span aria-label="Loading message" className="h-3 w-32 animate-pulse rounded-full bg-muted" />
                    : <>{KindIcon ? <KindIcon className="size-4 shrink-0" aria-label={kind === "poll" ? "Poll" : undefined} aria-hidden={kind === "poll" ? undefined : true} data-testid={`preview-${kind}`} /> : null}<span className="truncate">{mine && preview && !latest ? "You: " : ""}{!mine && preview && senderName ? `${senderName}: ` : ""}{preview}</span></>}
                </>}
            </span>
            <span className="flex shrink-0 items-center gap-1">
              {c.archived ? <Archive className="size-3.5 text-muted-foreground" aria-label="Archived" /> : null}
              {muted ? <BellOff className="size-3.5 text-muted-foreground" aria-label="Muted" /> : null}
              {c.pinned && !unread ? <Pin className="size-3.5 text-muted-foreground" aria-label="Pinned" /> : null}
              {statusMention ? <span className="flex h-5 items-center gap-0.5 rounded-full bg-primary/10 px-1.5 text-[11px] font-bold text-primary" title="Mentioned you in a status" aria-label="Mentioned you in a status" data-testid="status-mention-badge"><CircleDashed className="size-3" aria-hidden />@</span> : null}
              {unread && c.mentionCount ? <span className="flex size-5 items-center justify-center rounded-full bg-amber-500 text-[11px] font-bold text-white" data-testid="mention-badge">@</span> : null}
              {unread ? <span className="flex h-5 min-w-5 items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-bold text-primary-foreground" data-testid="unread-badge">{unread > 99 ? "99+" : unread}</span> : null}
            </span>
          </span>
        </span>
      </button>
      {ringed ? (
        <button type="button" aria-label={`View ${c.title}'s status`} data-testid="chat-status-ring" onClick={() => onOpenStatus!(statusRing!.principalId, c.title)}
          className="absolute left-2.5 top-1/2 size-12 -translate-y-1/2 rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" />
      ) : null}
    </li>
  );
});
