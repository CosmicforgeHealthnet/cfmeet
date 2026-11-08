"use client";

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

  if (mailboxes.isPending) return <div className="flex items-center gap-2"><Spinner /> Loading voicemail…</div>;
  if (mailboxes.error) return <p role="alert" className="text-sm text-destructive">{callingErrorMessage(mailboxes.error)}</p>;
  if (!mailboxes.data.mailboxes.length) {
    return (
      <div className="space-y-2 py-6 text-center">
        <Voicemail className="mx-auto size-8 text-muted-foreground" />
        <p className="font-medium">No voicemail box</p>
        <p className="text-sm text-muted-foreground">Voicemail boxes are set up by your organization&apos;s calling administrator. Messages left for you will appear here.</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        {mailboxes.data.mailboxes.length > 1 ? (
          <select aria-label="Mailbox" value={mailboxId} onChange={(e) => { setChosen(e.target.value); setPlaying(null); }} className="h-9 rounded-md border border-input bg-background px-3 text-sm">
            {mailboxes.data.mailboxes.map((m) => <option key={m.mailboxId} value={m.mailboxId}>{m.displayLabel}</option>)}
          </select>
        ) : <h2 className="font-semibold">{mailboxes.data.mailboxes[0].displayLabel}</h2>}
        <div className="ml-auto flex gap-2">
          <Button size="sm" variant={filter === "UNREAD" ? "default" : "outline"} onClick={() => setFilter(filter === "UNREAD" ? undefined : "UNREAD")}>Unread</Button>
          <Button size="icon" variant="ghost" aria-label="Refresh voicemail" onClick={() => void messages.refetch()}><RefreshCw className="size-4" /></Button>
        </div>
      </div>
      {mailboxes.data.truncated ? <p className="text-xs text-muted-foreground">Some shared mailboxes could not be checked. Ask your administrator if one is missing.</p> : null}
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {messages.isPending ? <Spinner /> : messages.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(messages.error)}</p> : list.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">{filter ? "No unread messages." : "No voicemail messages."}</p>
      ) : (
        <ul className="divide-y divide-border">
          {list.map((message) => (
            <li key={message.messageId} className="space-y-2 py-3">
              <div className="flex flex-wrap items-center gap-3">
                {message.readState === "UNREAD" ? <Mail className="size-4 text-primary" aria-label="Unread" /> : <MailOpen className="size-4 text-muted-foreground" aria-label="Read" />}
                <div className="min-w-0 flex-1">
                  <p className={`truncate text-sm ${message.readState === "UNREAD" ? "font-semibold" : ""}`}>{names.data?.[message.callerPrincipalId] ?? "CF Meet user"}</p>
                  <p className="text-xs text-muted-foreground">{new Date(message.receivedAt).toLocaleString()} {message.durationMs !== null ? `· ${duration(message.durationMs)}` : ""} {message.state !== "READY" ? `· ${message.state.toLowerCase()}` : ""}</p>
                </div>
                <Button size="sm" variant="outline" disabled={message.state !== "READY" || media.isPending} onClick={() => void play(message)}><Play className="size-4" /> Play</Button>
                <Button size="sm" variant="ghost" disabled={readState.isPending} onClick={() => readState.mutate({ messageId: message.messageId, body: { expectedVersion: message.version, readState: message.readState === "READ" ? "UNREAD" : "READ" } }, { onError: (e) => setError(callingErrorMessage(e)) })}>
                  Mark {message.readState === "READ" ? "unread" : "read"}
                </Button>
                <Button size="icon" variant="ghost" aria-label="Delete message" disabled={remove.isPending} onClick={() => {
                  if (!window.confirm("Delete this voicemail?")) return;
                  remove.mutate({ messageId: message.messageId, expectedVersion: message.version }, { onError: (e) => setError(callingErrorMessage(e)) });
                  if (playing?.messageId === message.messageId) setPlaying(null);
                }}><Trash2 className="size-4" /></Button>
              </div>
              {playing?.messageId === message.messageId ? (
                // A new short-lived signed URL is requested for each playback.
                <audio controls autoPlay src={playing.url} className="w-full" onError={() => setError("The recording could not be played. Try again.")} />
              ) : null}
            </li>
          ))}
        </ul>
      )}
      {messages.hasNextPage ? <Button variant="ghost" className="w-full" disabled={messages.isFetchingNextPage} onClick={() => void messages.fetchNextPage()}>Load more</Button> : null}
    </div>
  );
}
