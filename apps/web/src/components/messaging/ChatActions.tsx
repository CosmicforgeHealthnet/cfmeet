"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, Link2, Megaphone, Search, Send } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { Button } from "@/components/ui/button";
import type { ConversationView, MessageView, MessagingEngine } from "@/lib/messaging/engine";
import { usePeoplePicker } from "./people-picker";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { cn } from "@/lib/utils";

const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (k: string) => COLORS[([...k].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (n: string) => n.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

const FORWARD_LIMIT = 5;

type ForwardTarget = { key: string; name: string; subtitle: string | null; conversationId?: string; userId?: string; cfMeetId?: string };

/**
 * WhatsApp's "Forward message to": search, tick up to five chats or people (recent chats
 * first, then contacts), see what you're forwarding, add a message, send once.
 */
export function ForwardDialog({ engine, conversations, sourceId, message, onClose }: {
  engine: MessagingEngine;
  conversations: ConversationView[];
  sourceId: string;
  message: MessageView;
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [picked, setPicked] = useState<ForwardTarget[]>([]);
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const people = usePeoplePicker(true, q);
  const needle = q.trim().toLocaleLowerCase();
  const recent: ForwardTarget[] = conversations
    .filter((c) => c.id !== sourceId && c.lifecycleState === "ACTIVE" && c.securityProfile === "MANAGED" && !c.announcementOnly)
    .filter((c) => !needle || c.title.toLocaleLowerCase().includes(needle))
    .slice(0, 30)
    .map((c) => ({ key: `c:${c.id}`, name: c.title, subtitle: c.contextClass === "GROUP" ? `${c.members.length} members` : null, conversationId: c.id }));
  const recentNames = new Set(recent.map((r) => r.name));
  const contacts: ForwardTarget[] = people.people
    .filter((p) => !recentNames.has(p.name))
    .map((p) => ({ key: p.id, name: p.name, subtitle: null, ...(p.kind === "contact" ? { cfMeetId: p.value } : { userId: p.value }) }));
  const isPicked = (t: ForwardTarget) => picked.some((x) => x.key === t.key);
  const toggle = (t: ForwardTarget) => {
    setError(null);
    if (isPicked(t)) setPicked(picked.filter((x) => x.key !== t.key));
    else if (picked.length >= FORWARD_LIMIT) setError(`You can forward to up to ${FORWARD_LIMIT} chats at once.`);
    else setPicked([...picked, t]);
  };
  const send = useMutation({
    mutationFn: () => engine.forwardTo(sourceId, message, picked, note),
    onSuccess: () => onClose(),
    onError: (e) => setError(messagingErrorMessage(e)),
  });
  const row = (t: ForwardTarget) => (
    <li key={t.key}>
      <button type="button" role="checkbox" aria-checked={isPicked(t)} onClick={() => toggle(t)} data-testid="forward-target" data-name={t.name}
        className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left hover:bg-muted/60">
        <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-md border-2", isPicked(t) ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>
          {isPicked(t) ? <Check className="size-3.5" strokeWidth={3} /> : null}
        </span>
        <span className="flex size-10 shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white" style={{ backgroundColor: colorFor(t.key) }}>{initials(t.name)}</span>
        <span className="min-w-0 flex-1"><span className="block truncate text-[15px] font-medium">{t.name}</span>{t.subtitle ? <span className="block truncate text-xs text-muted-foreground">{t.subtitle}</span> : null}</span>
      </button>
    </li>
  );
  const what = message.file ? `${message.file.mediaType.startsWith("image/") ? "📷 " : message.file.mediaType.startsWith("audio/") ? "🎤 " : message.file.mediaType.startsWith("video/") ? "🎥 " : "📄 "}${message.file.name}` : message.text ?? "Message";
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="flex max-h-[min(720px,92dvh)] flex-col gap-0 p-0 sm:max-w-md" data-testid="forward-dialog">
        <DialogHeader className="border-b border-border px-5 pb-3 pt-5 text-left">
          <DialogTitle>Forward message to</DialogTitle>
          <DialogDescription className="sr-only">Choose up to {FORWARD_LIMIT} chats or people.</DialogDescription>
          <label className="mt-3 flex h-10 items-center gap-2 rounded-full bg-muted/70 px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
            <Search className="size-4" />
            <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search name" aria-label="Search chats and people" className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
          </label>
        </DialogHeader>
        <ul className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-3 py-2">
          {recent.length ? <li className="px-2 pb-1 pt-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Recent chats</li> : null}
          {recent.map(row)}
          {contacts.length ? <li className="px-2 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{people.heading}</li> : null}
          {contacts.map(row)}
          {!recent.length && !contacts.length ? <li className="px-2 py-8 text-center text-sm text-muted-foreground">{people.pending ? "Loading…" : "No chats or people match."}</li> : null}
        </ul>
        {error ? <p role="alert" className="mx-4 mb-2 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
        <div className="flex items-end gap-2 border-t border-border p-3">
          <div className="min-w-0 flex-1 space-y-1.5">
            <p className="truncate rounded-lg bg-muted/60 px-2.5 py-1.5 text-xs text-muted-foreground" data-testid="forward-preview">{what}</p>
            <input value={note} onChange={(e) => setNote(e.target.value)} maxLength={4000} placeholder="Add a message…" aria-label="Add a message"
              className="h-10 w-full rounded-full bg-muted/70 px-3.5 text-sm outline-none focus:ring-2 focus:ring-ring" data-testid="forward-note" />
          </div>
          <button type="button" disabled={!picked.length || send.isPending} onClick={() => { setError(null); send.mutate(); }} aria-label={`Forward to ${picked.length} chats`} data-testid="forward-send"
            className="relative flex size-11 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-40">
            <Send className="size-5" />
            {picked.length ? <span className="absolute -right-1 -top-1 flex size-5 items-center justify-center rounded-full bg-background text-[11px] font-bold text-primary ring-1 ring-border">{picked.length}</span> : null}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

const REASONS = [
  ["SPAM", "Spam"], ["HARASSMENT", "Harassment or bullying"], ["HATE", "Hate"], ["VIOLENCE", "Violence or threats"],
  ["SEXUAL_CONTENT", "Sexual content"], ["CHILD_SAFETY", "Child safety"], ["SELF_HARM", "Self-harm"],
  ["ILLEGAL_CONTENT", "Illegal content"], ["IMPERSONATION", "Pretending to be someone"],
] as const;

/** Report a message. In private chats the reported text is shared from this device (only that message). */
export function ReportDialog({ engine, conversationId, message, privateChat, onClose }: {
  engine: MessagingEngine;
  conversationId: string;
  /** The reported message; absent: the whole chat ("Report group"). */
  message?: { messageId: string; text: string | null };
  privateChat: boolean;
  onClose: () => void;
}) {
  const [reason, setReason] = useState<string>("SPAM");
  const [note, setNote] = useState("");
  const report = useMutation({ mutationFn: () => engine.reportMessages(conversationId, message ? [message] : [], reason, note.trim() || undefined) });
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{report.isSuccess ? "Thanks for reporting" : message ? "Report message" : "Report group"}</DialogTitle>
          <DialogDescription>{report.isSuccess ? "CF Meet will review it." : !message ? "CF Meet will review this group." : privateChat ? "This chat is private: reporting sends this one message to CF Meet so it can be reviewed." : "CF Meet will review this message."}</DialogDescription>
        </DialogHeader>
        {report.isSuccess ? <Button className="h-11 w-full rounded-xl" onClick={onClose}>Done</Button> : (
          <div className="space-y-3">
            <NiceSelect label="Reason" value={reason} onChange={setReason} options={REASONS.map(([value, label]) => ({ value, label }))} />
            <textarea value={note} onChange={(e) => setNote(e.target.value)} maxLength={500} rows={3} placeholder="Anything else? (optional)" aria-label="More details"
              className="w-full rounded-xl border border-transparent bg-muted/70 px-3 py-2 text-sm outline-none focus:border-primary focus:bg-background focus:ring-2 focus:ring-primary/20" />
            {report.isError ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{messagingErrorMessage(report.error)}</p> : null}
            <Button variant="destructive" className="h-11 w-full rounded-xl" loading={report.isPending} onClick={() => report.mutate()}>Report</Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** Group admin tools in Chat info: admins-only sending, invite links, hand over ownership. */
export function GroupAdminTools({ engine, conversation, names, me }: {
  engine: MessagingEngine;
  conversation: ConversationView;
  names: Record<string, string>;
  me: string | null;
}) {
  const client = useQueryClient();
  const key = ["chat-invite-links", conversation.id];
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [newOwner, setNewOwner] = useState("");
  const links = useQuery({ queryKey: key, queryFn: () => engine.inviteLinks(conversation.id), retry: false });
  const fail = (e: unknown) => setError(messagingErrorMessage(e));
  const toggle = useMutation({ mutationFn: (on: boolean) => engine.setAnnouncementOnly(conversation.id, on), onError: fail });
  const create = useMutation({
    mutationFn: () => engine.createInviteLink(conversation.id, { expiresInSeconds: 7 * 24 * 3600 }),
    onSuccess: (l) => { setCreated(`${window.location.origin}/dashboard/messages/join?token=${encodeURIComponent(l.token)}`); setCopied(false); void client.invalidateQueries({ queryKey: key }); },
    onError: fail,
  });
  const revoke = useMutation({ mutationFn: (id: string) => engine.revokeInviteLink(conversation.id, id), onSuccess: () => { setCreated(null); void client.invalidateQueries({ queryKey: key }); }, onError: fail });
  const transfer = useMutation({ mutationFn: () => engine.transferOwnership(conversation.id, newOwner), onSuccess: () => setNewOwner(""), onError: fail });
  const active = (links.data ?? []).filter((l) => l.status === "ACTIVE");
  const owner = conversation.myRole === "OWNER";
  const others = conversation.members.filter((p) => p !== me);

  return (
    <section className="space-y-4">
      {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
      <button type="button" role="switch" aria-checked={conversation.announcementOnly} disabled={toggle.isPending} onClick={() => toggle.mutate(!conversation.announcementOnly)}
        className="flex w-full items-center gap-3 rounded-xl px-1 py-1 text-left disabled:opacity-60">
        <Megaphone className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1"><span className="block text-sm font-medium">Only admins can send</span><span className="block text-xs text-muted-foreground">Everyone else can read and react.</span></span>
        <span aria-hidden className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", conversation.announcementOnly ? "bg-primary" : "bg-muted-foreground/30")}>
          <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", conversation.announcementOnly ? "left-[1.125rem]" : "left-0.5")} />
        </span>
      </button>

      {links.isError ? null : (
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Invite link</p>
          {created ? (
            <div className="space-y-2 rounded-xl bg-primary/5 p-3 ring-1 ring-primary/20">
              <p className="break-all text-xs">{created}</p>
              <Button size="sm" className="h-8 w-full rounded-xl" onClick={() => { void navigator.clipboard?.writeText(created); setCopied(true); }}>{copied ? <Check className="size-4" /> : <Copy className="size-4" />}{copied ? "Copied" : "Copy link"}</Button>
              <p className="text-[11px] text-muted-foreground">Works for 7 days. Copy it now: it&apos;s only shown once.</p>
            </div>
          ) : (
            <button type="button" disabled={create.isPending} onClick={() => create.mutate()} className="flex w-full items-center gap-3 rounded-xl px-1 py-1.5 text-left text-sm font-medium text-primary hover:bg-primary/5 disabled:opacity-60">
              <Link2 className="size-4" />Create invite link
            </button>
          )}
          {active.length ? (
            <ul className="mt-2 space-y-1">
              {active.map((l) => (
                <li key={l.inviteLinkId} className="flex items-center gap-2 text-xs text-muted-foreground">
                  <span className="min-w-0 flex-1 truncate">Link · used {l.useCount}{l.maxUses ? ` of ${l.maxUses}` : ""}{l.expiresAt ? ` · until ${new Date(l.expiresAt).toLocaleDateString([], { day: "numeric", month: "short" })}` : ""}</span>
                  <button type="button" disabled={revoke.isPending} onClick={() => revoke.mutate(l.inviteLinkId)} className="h-7 rounded-lg px-2 font-semibold text-destructive hover:bg-destructive/10">Turn off</button>
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      )}

      {owner && others.length ? (
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Group owner</p>
          <div className="flex gap-2">
            <NiceSelect className="min-w-0 flex-1" value={newOwner} onChange={setNewOwner} placeholder="Make someone else the owner"
              options={others.map((p) => ({ value: p, label: names[p] ?? "CF Meet user" }))} />
            <Button size="sm" className="h-11 rounded-xl" disabled={!newOwner || transfer.isPending} onClick={() => transfer.mutate()}>Hand over</Button>
          </div>
          <p className="mt-1 text-[11px] text-muted-foreground">You stay in the group as an admin and can then leave.</p>
        </div>
      ) : null}
    </section>
  );
}
