"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { communicationsService as hub } from "@/services/v2/communications.service";
import type { Contact } from "@/types/communications";
import type { MessagingEngine } from "@/lib/messaging/engine";
import { sharePrivately, type ShareResult } from "@/lib/messaging/private-shares";
import { communicationsErrorMessage } from "@/lib/communications/errors";

async function listAudience(id: string): Promise<Contact[]> {
  const result: Contact[] = [];
  let cursor: string | undefined;
  const seen = new Set<string>();
  do {
    const page = await hub.contactList(id, cursor);
    result.push(...page.members.filter((m) => m.relationship === "CONNECTED"));
    const next = page.nextCursor;
    if (next && seen.has(next)) throw new Error("The contact list could not be loaded completely");
    if (next) seen.add(next);
    cursor = next ?? undefined;
  } while (cursor && result.length <= 500);
  if (cursor || result.length > 500) throw new Error("The contact list is too large");
  return result;
}

export default function PrivateShares({ engine, mode, onClose }: {
  engine: MessagingEngine; mode: "status" | "broadcast"; onClose: () => void;
}) {
  const [listId, setListId] = useState("");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ShareResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const lists = useQuery({ queryKey: ["contact-lists"], queryFn: () => hub.contactLists(), retry: false });
  const audience = useQuery({ queryKey: ["private-share-audience", listId], queryFn: () => listAudience(listId), enabled: !!listId, retry: false });
  const status = mode === "status";
  const submit = async () => {
    if (busy || result || !audience.data?.length) return;
    setBusy(true);
    setError(null);
    try {
      setResult(await sharePrivately(engine, audience.data.map((c) => ({ cfMeetId: c.cfMeetId, label: c.alias || c.displayName })), text,
        status ? { postId: crypto.randomUUID(), createdAt: Date.now() } : null));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Unable to share right now");
    } finally { setBusy(false); }
  };
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
      <DialogContent className="sm:max-w-lg" onInteractOutside={(event) => { if (busy) event.preventDefault(); }} onEscapeKeyDown={(event) => { if (busy) event.preventDefault(); }}>
        <DialogHeader>
          <DialogTitle>{status ? "Post a status" : "Send a broadcast"}</DialogTitle>
          <DialogDescription>{status ? "Share an encrypted text status with a contact list. It disappears after 24 hours." : "Send an encrypted message separately to each person in a contact list. Replies stay in your private chats."}</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
          <label className="block space-y-1 text-sm font-medium">Audience
            <select className="h-11 w-full rounded-xl border bg-background px-3" value={listId} disabled={busy || !!result} onChange={(event) => setListId(event.target.value)}>
              <option value="">Choose a contact list</option>
              {lists.data?.data.map((list) => <option key={list.listId} value={list.listId}>{list.name} ({list.memberCount})</option>)}
            </select>
          </label>
          {lists.data?.data.length === 0 ? <p className="text-sm text-muted-foreground">Create a list under Contacts → Lists, then choose it here.</p> : null}
          {lists.isError || audience.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(lists.error ?? audience.error)}</p> : null}
          <label className="block space-y-1 text-sm font-medium">{status ? "Your status" : "Message"}
            <textarea className="min-h-32 w-full resize-y rounded-xl border bg-background p-3" value={text} maxLength={status ? 700 : 4000} disabled={busy || !!result} onChange={(event) => setText(event.target.value)} />
          </label>
          <p className="text-sm text-muted-foreground">Only you see the audience list. People you have blocked or removed cannot receive a new share.</p>
          {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
          <div aria-live="polite" className="text-sm">
            {busy ? "Sharing securely. Keep this window open…" : null}
            {result ? <>
              <p>Sent to {result.sent.length} {result.sent.length === 1 ? "person" : "people"}.</p>
              {result.unconfirmed.length ? <p className="mt-1 text-amber-700">Not confirmed for {result.unconfirmed.map((r) => r.label).join(", ")}. Check those chats before sending again.</p> : null}
            </> : null}
          </div>
          <div className="flex justify-end gap-2">
            <button type="button" disabled={busy} onClick={onClose} className="h-10 rounded-xl px-4 hover:bg-muted">{result ? "Done" : "Cancel"}</button>
            {!result ? <button type="submit" disabled={busy || !text.trim() || !audience.data?.length || audience.isFetching} className="h-10 rounded-xl bg-primary px-4 font-semibold text-primary-foreground disabled:opacity-50">{status ? "Post status" : "Send broadcast"}</button> : null}
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
