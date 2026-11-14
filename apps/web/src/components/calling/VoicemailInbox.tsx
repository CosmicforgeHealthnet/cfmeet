"use client";

import { Popover } from "radix-ui";
import { MoreHorizontal } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/**
 * VoicemailInbox — the signed-in user's own voicemail (CAL-006).
 *
 * Mailboxes: the adapter's owner-filtered GET /v1/enterprise/voicemail/mailboxes. Messages,
 * playback (POST …/media-access, at most 900 s signed URL, fetched per playback, SHA-256
 * checked and played from a blob: URL; the object-storage origin must be in connect-src and
 * allow this origin by CORS — a deployment setting), read state and delete all carry the user's principalId, so Calling answers with
 * the owner view and audits it as "on behalf of owner". No administrator view is offered here.
 */

import { useState } from "react";
import { Mail, MailOpen, Play, RefreshCw, Trash2, Voicemail } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useDeleteVoicemailMessage, useSetReadState, useVoicemailMailboxes, useVoicemailMediaAccess, useVoicemailMessages } from "@/hooks/calling";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { VoicemailMessage, VoicemailReadState } from "@/types/calling";

function duration(ms: number | null): string {
  if (ms === null) return "";
  const seconds = Math.round(ms / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

export default function VoicemailInbox() {
  const mailboxes = useVoicemailMailboxes();
  const [chosen, setChosen] = useState("");
  const mailboxId = chosen || mailboxes.data?.mailboxes[0]?.mailboxId || "";
  const [filter, setFilter] = useState<VoicemailReadState | undefined>(undefined);
  const messages = useVoicemailMessages({ mailboxId, readState: filter, limit: 25 });
  const list = messages.data?.pages.flatMap((p) => p.messages) ?? [];
  const names = usePrincipalNames(list.map((m) => m.callerPrincipalId));
  const readState = useSetReadState();
  const remove = useDeleteVoicemailMessage();
  const media = useVoicemailMediaAccess();
  const [playing, setPlaying] = useState<{ messageId: string; url: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const play = async (message: VoicemailMessage) => {
    setError(null);
    try {
      const access = await media.mutateAsync(message.messageId);
      // Fetched (connect-src) and played from a blob: URL (media-src), after checking the
      // SHA-256 Calling reported; the signed URL itself is never rendered or stored.
      const audio = await fetch(access.url, { credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" });
      if (!audio.ok) throw new Error("The recording could not be downloaded. Try again.");
      const bytes = await audio.arrayBuffer();
      const digest = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (b) => b.toString(16).padStart(2, "0")).join("");
      if (access.sha256 && digest !== access.sha256.toLowerCase()) throw new Error("The recording failed its integrity check and was not played.");
      if (playing) URL.revokeObjectURL(playing.url);
      setPlaying({ messageId: message.messageId, url: URL.createObjectURL(new Blob([bytes], { type: access.contentType })) });
      if (message.readState === "UNREAD") {
        await readState.mutateAsync({ messageId: message.messageId, body: { expectedVersion: message.version, readState: "READ" } });
      }
    } catch (e) { setError(callingErrorMessage(e)); }
  };

  const [deleting, setDeleting] = useState<VoicemailMessage | null>(null);
  const unread = list.filter((m) => m.readState === "UNREAD").length;

  if (mailboxes.isPending) return <div className="space-y-2 pt-2">{[0, 1, 2].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</div>;
  if (mailboxes.error) return <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{callingErrorMessage(mailboxes.error)}</p>;
  if (!mailboxes.data.mailboxes.length) {
    return (
      <div className="flex flex-col items-center px-4 py-12 text-center">
        <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Voicemail className="size-6" /></span>
        <p className="mt-4 font-semibold">No voicemail box yet</p>
        <p className="mt-1 text-sm text-muted-foreground">Your organization&apos;s calling admin sets these up. Messages left for you will show here.</p>
      </div>
    );
  }

  return (
    <div className="space-y-3 pt-1">
      <div className="flex items-center gap-2">
        {mailboxes.data.mailboxes.length > 1 ? (
          <NiceSelect className="min-w-0 flex-1" value={mailboxId} onChange={(v) => { setChosen(v); setPlaying(null); }} options={mailboxes.data.mailboxes.map((m) => ({ value: m.mailboxId, label: m.displayLabel }))} />
        ) : <p className="min-w-0 flex-1 truncate text-sm font-semibold">{mailboxes.data.mailboxes[0].displayLabel}</p>}
        <button type="button" aria-pressed={filter === "UNREAD"} onClick={() => setFilter(filter === "UNREAD" ? undefined : "UNREAD")} className={cn("h-8 shrink-0 rounded-full px-3 text-xs font-semibold transition-colors", filter === "UNREAD" ? "bg-primary/10 text-primary" : "bg-muted/60 text-muted-foreground hover:bg-muted")}>Unread{unread ? ` ${unread}` : ""}</button>
        <button type="button" aria-label="Refresh voicemail" title="Refresh" onClick={() => void messages.refetch()} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><RefreshCw className={cn("size-4", messages.isFetching && "animate-spin")} /></button>
      </div>
      {mailboxes.data.truncated ? <p className="text-xs text-muted-foreground">Some shared mailboxes couldn&apos;t be checked. Ask your admin if one is missing.</p> : null}
      {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
      {messages.isPending ? <div className="space-y-2">{[0, 1].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</div> : messages.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(messages.error)}</p> : list.length === 0 ? (
        <div className="flex flex-col items-center px-4 py-10 text-center">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Voicemail className="size-5" /></span>
          <p className="mt-3 font-semibold">{filter ? "All caught up" : "No voicemail"}</p>
          <p className="mt-1 text-sm text-muted-foreground">{filter ? "You've listened to everything." : "When someone leaves a message, it shows up here."}</p>
        </div>
      ) : (
        <ul className="space-y-1">
          {list.map((message) => {
            const unreadMsg = message.readState === "UNREAD";
            const name = names.data?.[message.callerPrincipalId] ?? "CF Meet user";
            const isPlaying = playing?.messageId === message.messageId;
            return (
              <li key={message.messageId} className={cn("rounded-2xl px-2.5 py-2.5 transition-colors", isPlaying ? "bg-primary/5" : "hover:bg-muted/60")}>
                <div className="flex items-center gap-3">
                  <button type="button" aria-label={`Play message from ${name}`} disabled={message.state !== "READY" || media.isPending} onClick={() => void play(message)}
                    className={cn("relative flex size-11 shrink-0 items-center justify-center rounded-full transition-colors disabled:opacity-40", isPlaying ? "bg-primary text-primary-foreground" : "bg-primary/10 text-primary hover:bg-primary/15")}>
                    <Play className="size-5" />
                    {unreadMsg && <span className="absolute -right-0.5 -top-0.5 size-3 rounded-full bg-primary ring-2 ring-background" aria-label="Unread" />}
                  </button>
                  <div className="min-w-0 flex-1">
                    <p className={cn("truncate text-[15px]", unreadMsg ? "font-bold" : "font-semibold")}>{name}</p>
                    <p className="truncate text-xs text-muted-foreground">
                      {new Date(message.receivedAt).toLocaleString([], { day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })}
                      {message.durationMs !== null ? ` · ${duration(message.durationMs)}` : ""}
                      {message.state !== "READY" ? ` · ${message.state.toLowerCase()}` : ""}
                    </p>
                  </div>
                  <Popover.Root>
                    <Popover.Trigger asChild><button type="button" aria-label="More" className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><MoreHorizontal className="size-4" /></button></Popover.Trigger>
                    <Popover.Portal>
                      <Popover.Content align="end" sideOffset={4} className="z-50 w-48 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                        <Popover.Close asChild>
                          <button type="button" disabled={readState.isPending} onClick={() => readState.mutate({ messageId: message.messageId, body: { expectedVersion: message.version, readState: message.readState === "READ" ? "UNREAD" : "READ" } }, { onError: (e) => setError(callingErrorMessage(e)) })} className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted">
                            {unreadMsg ? <MailOpen className="size-4 text-muted-foreground" /> : <Mail className="size-4 text-muted-foreground" />}Mark as {unreadMsg ? "heard" : "new"}
                          </button>
                        </Popover.Close>
                        <Popover.Close asChild>
                          <button type="button" onClick={() => setDeleting(message)} className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-destructive hover:bg-muted"><Trash2 className="size-4" />Delete</button>
                        </Popover.Close>
                      </Popover.Content>
                    </Popover.Portal>
                  </Popover.Root>
                </div>
                {isPlaying ? (
                  // A new short-lived signed URL is requested for each playback.
                  <audio controls autoPlay src={playing.url} className="mt-2 h-10 w-full" onError={() => setError("The recording couldn't be played. Try again.")} />
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {messages.hasNextPage ? <button type="button" disabled={messages.isFetchingNextPage} onClick={() => void messages.fetchNextPage()} className="mx-auto block h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-50">{messages.isFetchingNextPage ? "Loading…" : "Show older messages"}</button> : null}

      <Dialog open={!!deleting} onOpenChange={(v) => !v && setDeleting(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader className="text-left"><DialogTitle>Delete this voicemail?</DialogTitle><DialogDescription>It&apos;s removed for good.</DialogDescription></DialogHeader>
          <div className="flex justify-end gap-2 pt-2">
            <button type="button" onClick={() => setDeleting(null)} className="h-10 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" disabled={remove.isPending} onClick={() => {
              if (!deleting) return;
              remove.mutate({ messageId: deleting.messageId, expectedVersion: deleting.version }, { onError: (e) => setError(callingErrorMessage(e)) });
              if (playing?.messageId === deleting.messageId) setPlaying(null);
              setDeleting(null);
            }} className="h-10 rounded-full bg-destructive px-5 text-sm font-semibold text-white hover:bg-destructive/90">Delete</button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );

}
