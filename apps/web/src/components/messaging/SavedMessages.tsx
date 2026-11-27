"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Star, StarOff } from "lucide-react";
import { SidePage } from "@/components/ui/side-page";
import { communicationsService as hub } from "@/services/v2/communications.service";

export const SAVED_KEY = ["saved-messages"] as const;

/**
 * Starred messages (WhatsApp): every message you starred, newest first. The server keeps only
 * which message it is; its text comes from the chat when it's loaded on this device.
 */
export default function SavedMessages({ open, onOpenChange, chatTitle, messageText, onOpenChat }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  chatTitle: (conversationId: string) => string;
  messageText: (conversationId: string, messageId: string) => string | null;
  onOpenChat: (conversationId: string) => void;
}) {
  const client = useQueryClient();
  const saved = useQuery({ queryKey: SAVED_KEY, queryFn: () => hub.savedMessages(), retry: false, enabled: open });
  const unstar = useMutation({ mutationFn: (savedId: string) => hub.unsaveMessage(savedId), onSuccess: () => client.invalidateQueries({ queryKey: SAVED_KEY }) });
  const items = [...(saved.data?.data ?? [])].sort((a, b) => b.savedAt.localeCompare(a.savedAt));

  return (
    <SidePage open={open} onOpenChange={onOpenChange} title="Starred messages" description="Messages you starred">
      {saved.isLoading ? <div className="space-y-2 p-4">{[0, 1, 2].map((i) => <div key={i} className="h-16 animate-pulse rounded-xl bg-muted" />)}</div> : null}
      {saved.isError ? <p className="p-4 text-sm text-destructive">Starred messages couldn&apos;t load.</p> : null}
      {saved.data && items.length === 0 ? (
        <div className="flex flex-col items-center px-8 py-16 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-amber-400/15 text-amber-500"><Star className="size-6" /></span>
          <p className="mt-4 font-semibold">No starred messages</p>
          <p className="mt-1 text-sm text-muted-foreground">Tap a message&apos;s menu and choose Star to keep it here.</p>
        </div>
      ) : null}
      <ul className="divide-y divide-border">
        {items.map((x) => {
          const text = x.available ? messageText(x.conversationId, x.messageId) : null;
          return (
            <li key={x.savedId} className="flex items-start gap-3 px-4 py-3">
              <button type="button" disabled={!x.available} onClick={() => { onOpenChat(x.conversationId); onOpenChange(false); }} className="min-w-0 flex-1 text-left disabled:cursor-default">
                <span className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
                  <span className="truncate font-semibold text-foreground">{chatTitle(x.conversationId)}</span>
                  <span className="shrink-0">{new Date(x.savedAt).toLocaleDateString([], { day: "numeric", month: "short" })}</span>
                </span>
                <span className="mt-1 line-clamp-3 block text-sm">{!x.available ? <span className="italic text-muted-foreground">This message is no longer available</span> : text ?? <span className="text-muted-foreground">Open the chat to see this message</span>}</span>
              </button>
              <button type="button" aria-label="Unstar" title="Unstar" disabled={unstar.isPending} onClick={() => unstar.mutate(x.savedId)} className="flex size-9 shrink-0 items-center justify-center rounded-full text-amber-500 hover:bg-muted"><StarOff className="size-4" /></button>
            </li>
          );
        })}
      </ul>
    </SidePage>
  );
}
