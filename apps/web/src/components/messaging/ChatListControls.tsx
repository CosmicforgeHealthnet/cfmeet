"use client";

/**
 * F1 (WhatsApp): archive and pin a chat for yourself. The toggles sit in Chat info next to
 * the chat's notifications; the chat list shows an "Archived" row and pinned chats first.
 */
import { useState } from "react";
import { Archive, ArchiveRestore, ArrowLeft, Pin, PinOff, Timer } from "lucide-react";
import type { ConversationView, MessagingEngine } from "@/lib/messaging/engine";
import { MAX_PINNED_CHATS } from "@/lib/messaging/chat-list";
import { MessagingApiError, messagingErrorMessage } from "@/lib/messaging/errors";
import { NiceSelect } from "@/components/ui/nice-select";

const action = "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm font-medium hover:bg-muted disabled:opacity-50";

/** Archive / pin toggles for one chat (Chat info). */
export function ChatListToggles({ engine, conversation, pinnedTotal }: { engine: MessagingEngine; conversation: ConversationView; pinnedTotal: number }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const change = (next: { archived?: boolean; pinned?: boolean }) => {
    setBusy(true);
    setError(null);
    engine.setInboxSettings(conversation.id, next)
      .catch((e) => setError(e instanceof MessagingApiError && e.code === "PIN_LIMIT_REACHED" ? `You can pin up to ${MAX_PINNED_CHATS} chats.` : messagingErrorMessage(e)))
      .finally(() => setBusy(false));
  };
  const pinFull = !conversation.pinned && pinnedTotal >= MAX_PINNED_CHATS;
  return (
    <section data-testid="chat-list-settings">
      <button type="button" className={action} disabled={busy || (pinFull && !conversation.archived)} data-testid="pin-chat" onClick={() => change({ pinned: !conversation.pinned })}
        title={pinFull ? `You can pin up to ${MAX_PINNED_CHATS} chats` : undefined}>
        {conversation.pinned ? <PinOff className="size-4 text-muted-foreground" /> : <Pin className="size-4 text-muted-foreground" />}{conversation.pinned ? "Unpin chat" : "Pin chat"}
      </button>
      <button type="button" className={action} disabled={busy} data-testid="archive-chat" onClick={() => change({ archived: !conversation.archived })}>
        {conversation.archived ? <ArchiveRestore className="size-4 text-muted-foreground" /> : <Archive className="size-4 text-muted-foreground" />}{conversation.archived ? "Unarchive chat" : "Archive chat"}
      </button>
      {error ? <p role="alert" className="px-3 text-xs text-destructive">{error}</p> : null}
    </section>
  );
}

/** The "Archived" row at the top of the chat list (hidden while nothing is archived). */
export function ArchivedRow({ count, unread, onOpen }: { count: number; unread: number; onOpen: () => void }) {
  if (!count) return null;
  return (
    <li>
      <button type="button" data-testid="archived-chats" onClick={onOpen} className="flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors hover:bg-muted/70">
        <span className="flex size-12 items-center justify-center"><Archive className="size-5 text-muted-foreground" /></span>
        <span className="min-w-0 flex-1 text-[15px] font-semibold">Archived</span>
        <span className={unread ? "text-xs font-semibold text-primary" : "text-xs text-muted-foreground"}>{unread || count}</span>
      </button>
    </li>
  );
}

/** The header of the Archived list (back to all chats). */
export function ArchivedHeader({ onBack }: { onBack: () => void }) {
  return (
    <li className="flex items-center gap-2 px-1 pb-1 pt-1">
      <button type="button" aria-label="Back to chats" data-testid="archived-back" onClick={onBack} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ArrowLeft className="size-5" /></button>
      <span className="text-sm font-semibold">Archived</span>
    </li>
  );
}

const DAY = 86_400;
const TIMERS = [["off", "Off"], [String(DAY), "24 hours"], [String(7 * DAY), "7 days"], [String(90 * DAY), "90 days"]] as const;
type Timer = (typeof TIMERS)[number][0];

/**
 * B4 (WhatsApp): disappearing messages. A chat admin (a group's owner or admin, either
 * person in a one-to-one chat) turns it on, changes it or turns it off; messages already
 * sent keep the timer they were sent with.
 */
export function DisappearingMessages({ engine, conversation, canChange }: { engine: MessagingEngine; conversation: ConversationView; canChange: boolean }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = conversation.retentionSeconds;
  const known = TIMERS.find(([v]) => (v === "off" ? current === null : Number(v) === current));
  const value: Timer | "" = known ? known[0] : "";
  const options = TIMERS.map(([v, label]) => ({ value: v as Timer, label }));
  const change = (next: Timer) => {
    if (next === value) return;
    setBusy(true);
    setError(null);
    engine.setDisappearing(conversation.id, next === "off" ? null : Number(next))
      .catch((e) => setError(e instanceof MessagingApiError && e.reason === "RETENTION_LOOSER_THAN_POLICY" ? "Your organization keeps messages for less time than this." : messagingErrorMessage(e)))
      .finally(() => setBusy(false));
  };
  if (!canChange && current === null) return null;
  return (
    <section data-testid="disappearing-messages" className="px-3">
      <p className="mb-1.5 flex items-center gap-2 text-sm font-medium"><Timer className="size-4 text-muted-foreground" />Disappearing messages</p>
      {canChange
        ? <NiceSelect value={value} onChange={change} disabled={busy} options={options} placeholder={current ? `${Math.round(current / 3600)} hours` : "Off"} />
        : <p className="text-sm text-muted-foreground">{known ? known[1] : `${Math.round((current ?? 0) / 3600)} hours`}</p>}
      <p className="mt-1.5 text-xs text-muted-foreground">New messages disappear after this time.</p>
      {error ? <p role="alert" className="mt-1 text-xs text-destructive">{error}</p> : null}
    </section>
  );
}
