"use client";

/**
 * Organization Messages (final closure, J-10): the conversations of the selected
 * organization workspace, in the organization's OWN Messaging scope. They are separate
 * from personal conversations, MANAGED (readable by CF Meet's servers, and labelled so),
 * and only active members of an active organization whose policy allows Messaging reach
 * them — the server decides every request; this screen only explains its answers.
 * Message text is rendered only as text (React escapes it).
 *
 * Same layout as personal Messages: chat list left, open chat right; separate screens on phones.
 */
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Building2, Check, LockOpen, MessageSquarePlus, Search, SendHorizontal, X } from "lucide-react";

import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { useAccountProfile } from "@/hooks/useAccount";
import { MessagingApiError } from "@/lib/messaging/errors";
import {
  addableColleagues,
  colleagueLabel,
  organizationMessagingProblem,
  type OrganizationMember,
} from "@/lib/messaging/organization";
import { businessOrganizationsService } from "@/services/v2/business-organizations.service";
import { organizationMessaging, type OrganizationMessage } from "@/services/v2/organization-messaging.service";

function problemOf(error: unknown): string {
  if (error instanceof MessagingApiError) {
    return organizationMessagingProblem(error.code, error.status) ?? error.message;
  }
  const status = (error as { response?: { status?: number; data?: { error?: { code?: string } } } })?.response;
  return organizationMessagingProblem(status?.data?.error?.code, status?.status ?? 0) ?? "The request failed. Please try again.";
}

const AVATAR_COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
function Avatar({ name, seed, size = "size-11", className }: { name: string; seed?: string; size?: string; className?: string }) {
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size, className)} style={{ backgroundColor: colorFor(seed ?? name) }}>{initials(name)}</span>;
}
const sameDay = (a: string, b: string) => new Date(a).toDateString() === new Date(b).toDateString();
function dayLabel(iso: string) {
  const d = new Date(iso), today = new Date(), yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, today.getTime() - d.getTime() < 6 * 86400000 ? { weekday: "long" } : { weekday: "short", day: "numeric", month: "short" });
}
function listTime(iso: string) {
  const d = new Date(iso), now = new Date();
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return now.getTime() - d.getTime() < 6 * 86400000 ? d.toLocaleDateString([], { weekday: "short" }) : d.toLocaleDateString([], { day: "numeric", month: "short" });
}
const clock = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

export default function OrganizationMessages({ organizationId, organizationName }: { organizationId: string; organizationName?: string | null }) {
  const api = useMemo(() => organizationMessaging(organizationId), [organizationId]);
  const queryClient = useQueryClient();
  const me = useAccountProfile().data?.userId ?? null;
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [search, setSearch] = useState("");
  const [peopleSearch, setPeopleSearch] = useState("");
  const bottom = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const keys = {
    conversations: ["org-messaging", organizationId, "conversations"] as const,
    messages: (id: string) => ["org-messaging", organizationId, "messages", id] as const,
  };

  const conversations = useQuery({ queryKey: keys.conversations, queryFn: () => api.conversations(), retry: false, refetchInterval: 15_000 });
  const messages = useQuery({
    queryKey: keys.messages(selected ?? "none"),
    queryFn: () => api.messages(selected as string),
    enabled: !!selected,
    retry: false,
    refetchInterval: 5_000,
  });
  const members = useQuery({
    queryKey: ["workspace", organizationId, "members"],
    queryFn: () => businessOrganizationsService.members(organizationId),
    retry: false,
  });
  // Message senders are messaging principals; map them back to member names (and spot "me").
  const memberList = (members.data ?? []) as OrganizationMember[];
  const userIds = memberList.map((m) => m.userId).filter(Boolean);
  const principals = useQuery({
    queryKey: ["org-messaging", organizationId, "principals", userIds.join()],
    queryFn: () => api.resolveMembers(userIds),
    enabled: userIds.length > 0,
    retry: false,
    staleTime: 5 * 60_000,
  });
  const nameByPrincipal = useMemo(() => {
    const byUser = new Map(memberList.map((m) => [m.userId, colleagueLabel(m)]));
    return new Map((principals.data?.people ?? []).map((p) => [p.principalId, byUser.get(p.userId) ?? "Colleague"]));
  }, [memberList, principals.data]);
  const myPrincipal = principals.data?.people.find((p) => p.userId === me)?.principalId ?? null;

  const send = useMutation({
    mutationFn: (text: string) => api.send(selected as string, text),
    onSuccess: () => {
      setDraft("");
      void queryClient.invalidateQueries({ queryKey: keys.messages(selected as string) });
    },
  });
  const closeCreate = () => { setCreating(false); setTitle(""); setPicked([]); setPeopleSearch(""); create.reset(); };
  const create = useMutation({
    mutationFn: async () => {
      const conversation = await api.createGroup(title.trim() || "Team conversation");
      if (picked.length) {
        const { people } = await api.resolveMembers(picked);
        for (const p of people) await api.addMember(conversation.conversationId, p.principalId);
      }
      return conversation;
    },
    onSuccess: (conversation) => {
      closeCreate();
      setSelected(conversation.conversationId);
      void queryClient.invalidateQueries({ queryKey: keys.conversations });
    },
  });

  const blocking = conversations.error ? problemOf(conversations.error) : null;
  const colleagues = addableColleagues(memberList, me).filter((c) => !peopleSearch.trim() || colleagueLabel(c).toLowerCase().includes(peopleSearch.trim().toLowerCase()));
  const list = [...(conversations.data?.conversations ?? [])]
    .filter((c) => !search.trim() || (c.title || "Conversation").toLowerCase().includes(search.trim().toLowerCase()))
    .sort((a, b) => (b.lastMessageAt ?? "").localeCompare(a.lastMessageAt ?? ""));
  const current = (conversations.data?.conversations ?? []).find((c) => c.conversationId === selected) ?? null;
  const currentTitle = current?.title || "Conversation";
  const ordered: OrganizationMessage[] = [...(messages.data?.messages ?? [])].sort((a, b) => a.sequence - b.sequence);
  const org = organizationName ?? "Organization";

  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); }, [ordered.length, selected]);
  useEffect(() => {
    const el = composer.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [draft]);
  const submit = () => { if (draft.trim() && !send.isPending) send.mutate(draft.trim()); };
  const iconBtn = "flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground";

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background" data-testid="organization-messages">
      {blocking ? (
        <div className="flex flex-1 flex-col items-center justify-center px-6 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Building2 className="size-6" /></span>
          <p className="mt-4 font-semibold">{org} messages aren&apos;t available</p>
          <p role="alert" className="mt-1 max-w-sm text-sm text-muted-foreground" data-testid="organization-messages-problem">{blocking}</p>
        </div>
      ) : (
        <div className="flex min-h-0 flex-1">
          <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[22rem] md:shrink-0", selected && "hidden md:flex")} aria-label="Conversations">
            <div className="space-y-3 px-4 pb-2 pt-4">
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0">
                  <h1 className="text-2xl font-bold tracking-tight">Chats</h1>
                  <p className="flex items-center gap-1 truncate text-xs text-muted-foreground"><Building2 className="size-3.5 shrink-0" />{org}</p>
                </div>
                <button type="button" aria-label="New group" title="New group" onClick={() => setCreating(true)} data-testid="organization-new-conversation" className="flex size-10 items-center justify-center rounded-full bg-primary/10 text-primary hover:bg-primary/15"><MessageSquarePlus className="size-5" /></button>
              </div>
              <label className="flex h-10 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
                <Search className="size-4 shrink-0" />
                <input type="search" aria-label="Search conversations" placeholder="Search chats" value={search} onChange={(e) => setSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
              </label>
              <p className="flex items-start gap-1.5 rounded-xl bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-300" data-testid="organization-messages-notice">
                <LockOpen className="mt-px size-3.5 shrink-0" />Work chats follow {org}&apos;s policy, and CF Meet can read them. They&apos;re kept apart from your personal chats.
              </p>
            </div>
            <ul className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-2 pb-2" data-testid="organization-conversation-list">
              {conversations.isLoading && [0, 1, 2].map((i) => <li key={i} className="flex items-center gap-3 px-3 py-3"><span className="size-12 animate-pulse rounded-full bg-muted" /><span className="flex-1 space-y-2"><span className="block h-3.5 w-1/2 animate-pulse rounded bg-muted" /><span className="block h-3 w-1/3 animate-pulse rounded bg-muted" /></span></li>)}
              {list.map((c) => {
                const name = c.title || "Conversation";
                const active = selected === c.conversationId;
                return (
                  <li key={c.conversationId}>
                    <button type="button" onClick={() => setSelected(c.conversationId)} aria-current={active ? "true" : undefined}
                      className={cn("flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors", active ? "bg-primary/10" : "hover:bg-muted/70")}>
                      <Avatar name={name} seed={c.conversationId} size="size-12" />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-baseline justify-between gap-2">
                          <span className="truncate text-[15px] font-semibold">{name}</span>
                          {c.lastMessageAt ? <time className="shrink-0 text-xs text-muted-foreground">{listTime(c.lastMessageAt)}</time> : null}
                        </span>
                        <span className="block truncate text-sm text-muted-foreground">{c.contextClass === "GROUP" ? "Group" : "Chat"}</span>
                      </span>
                    </button>
                  </li>
                );
              })}
              {!conversations.isLoading && list.length === 0 && (
                <li className="flex flex-col items-center px-6 py-14 text-center">
                  <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><MessageSquarePlus className="size-6" /></span>
                  <p className="mt-4 font-semibold">{search ? "No chats match" : "No work chats yet"}</p>
                  <p className="mt-1 text-sm text-muted-foreground">{search ? "Try another name." : `Start a group with people in ${org}.`}</p>
                  {!search && <button type="button" onClick={() => setCreating(true)} className="mt-4 h-10 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">New group</button>}
                </li>
              )}
            </ul>
          </aside>

          <main className={cn("flex min-w-0 min-h-0 flex-1 flex-col", !selected && "hidden md:flex")}>
            {!selected ? (
              <div className="m-auto max-w-sm px-6 text-center">
                <span className="mx-auto flex size-20 items-center justify-center rounded-[1.75rem] bg-primary/10 text-primary"><Building2 className="size-9" /></span>
                <h2 className="mt-5 text-xl font-semibold">{org} chats</h2>
                <p className="mt-2 text-sm text-muted-foreground">Pick a chat on the left, or start a group with your colleagues.</p>
              </div>
            ) : (
              <>
                <header className="flex h-16 shrink-0 items-center gap-2 border-b border-border px-2 sm:px-4">
                  <button type="button" className={cn(iconBtn, "md:hidden")} onClick={() => setSelected(null)} aria-label="Back to conversations"><ArrowLeft className="size-5" /></button>
                  <Avatar name={currentTitle} seed={selected} size="size-10" />
                  <span className="min-w-0">
                    <span className="block truncate font-semibold">{currentTitle}</span>
                    <span className="flex items-center gap-1 text-xs text-amber-700 dark:text-amber-400"><LockOpen className="size-3" />Work chat · CF Meet can read it</span>
                  </span>
                </header>
                <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-3 py-2 sm:px-6">
                  <ol className="mx-auto max-w-3xl pb-2" data-testid="organization-message-list">
                    {messages.error && <li role="alert" className="my-3 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{problemOf(messages.error)}</li>}
                    {messages.isLoading && <li className="flex justify-center py-10"><Spinner /></li>}
                    {!messages.isLoading && !messages.error && ordered.length === 0 && (
                      <li className="flex flex-col items-center px-6 py-16 text-center">
                        <Avatar name={currentTitle} seed={selected} size="size-16" className="text-xl" />
                        <p className="mt-3 font-semibold">{currentTitle}</p>
                        <p className="mt-1 text-sm text-muted-foreground">Say hello to start the conversation.</p>
                      </li>
                    )}
                    {ordered.map((m, i) => {
                      const prev = ordered[i - 1], next = ordered[i + 1];
                      const mine = !!myPrincipal && m.senderPrincipalId === myPrincipal;
                      const name = mine ? "You" : nameByPrincipal.get(m.senderPrincipalId) ?? "Colleague";
                      const separator = !prev || !sameDay(prev.createdAt, m.createdAt);
                      const near = (o?: OrganizationMessage) => !!o && o.senderPrincipalId === m.senderPrincipalId && sameDay(o.createdAt, m.createdAt) && Math.abs(Date.parse(o.createdAt) - Date.parse(m.createdAt)) < 5 * 60000;
                      const first = separator || !near(prev);
                      const last = !near(next);
                      const removed = m.lifecycle && m.lifecycle !== "ACTIVE";
                      return (
                        <Fragment key={m.messageId}>
                          {separator && <li className="sticky top-1 z-10 my-3 flex justify-center"><span className="rounded-full bg-background/90 px-3 py-1 text-xs font-medium text-muted-foreground shadow-sm ring-1 ring-border backdrop-blur">{dayLabel(m.createdAt)}</span></li>}
                          <li className={cn("flex items-end gap-2", mine ? "flex-row-reverse" : "flex-row", first ? "mt-3" : "mt-0.5")}>
                            {!mine && (last ? <Avatar name={name} seed={m.senderPrincipalId} size="size-7" className="mb-0.5 text-[10px]" /> : <span className="w-7 shrink-0" />)}
                            <div className={cn("flex min-w-0 max-w-[min(78%,34rem)] flex-col", mine ? "items-end" : "items-start")}>
                              {!mine && first && <p className="mb-1 px-3 text-xs font-semibold" style={{ color: colorFor(m.senderPrincipalId) }}>{name}</p>}
                              <div className={cn("rounded-[20px] px-3.5 py-2 text-[15px] leading-snug", mine ? "bg-primary text-primary-foreground" : "bg-muted text-foreground",
                                mine ? cn(!first && "rounded-tr-md", !last && "rounded-br-md") : cn(!first && "rounded-tl-md", !last && "rounded-bl-md"))}>
                                {removed ? <p className="italic opacity-75">Message deleted</p> : <p className="whitespace-pre-wrap break-words">{m.managed?.text ?? ""}</p>}
                                <p className={cn("mt-0.5 text-right text-[11px] tabular-nums", mine ? "text-primary-foreground/70" : "text-muted-foreground")}>{clock(m.createdAt)}</p>
                              </div>
                            </div>
                          </li>
                        </Fragment>
                      );
                    })}
                  </ol>
                  <div ref={bottom} />
                </div>
                {send.error && <p role="alert" className="mx-3 mb-2 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive sm:mx-6">{problemOf(send.error)}</p>}
                <div className="shrink-0 px-3 pb-3 sm:px-6 sm:pb-4">
                  <form className="mx-auto flex max-w-3xl items-end gap-1.5 rounded-[26px] border border-border bg-card p-1.5 pl-4 shadow-sm focus-within:border-primary/50" onSubmit={(e) => { e.preventDefault(); submit(); }}>
                    <textarea ref={composer} rows={1} value={draft} onChange={(e) => setDraft(e.target.value)} placeholder="Message" maxLength={4000} aria-label="Message"
                      onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); submit(); } }}
                      className="max-h-40 min-h-10 min-w-0 flex-1 resize-none bg-transparent py-2.5 text-[15px] leading-5 outline-none placeholder:text-muted-foreground" />
                    <button type="submit" disabled={send.isPending || !draft.trim()} aria-label="Send" className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30">
                      {send.isPending ? <Spinner /> : <SendHorizontal className="size-5" />}
                    </button>
                  </form>
                </div>
              </>
            )}
          </main>
        </div>
      )}

      <Dialog open={creating} onOpenChange={(v) => (v ? setCreating(true) : closeCreate())}>
        <DialogContent className="gap-0 p-0 sm:max-w-md">
          <DialogHeader className="border-b border-border px-5 pb-4 pt-5 text-left">
            <DialogTitle>New group</DialogTitle>
            <DialogDescription>With people in {org}</DialogDescription>
          </DialogHeader>
          <form onSubmit={(e) => { e.preventDefault(); create.mutate(); }}>
            <div className="space-y-3 px-5 pt-4">
              <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Group name, e.g. Design team" maxLength={120} aria-label="Conversation name"
                className="h-11 w-full rounded-xl border border-border bg-background px-3 text-sm font-medium outline-none focus:border-primary" />
              <label className="flex h-10 items-center gap-2 rounded-full bg-muted px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
                <Search className="size-4 shrink-0" />
                <input aria-label="Search colleagues" placeholder="Search colleagues" value={peopleSearch} onChange={(e) => setPeopleSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
              </label>
              {picked.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {picked.map((id) => {
                    const c = memberList.find((m) => m.userId === id);
                    const label = c ? colleagueLabel(c) : "Colleague";
                    return <button key={id} type="button" onClick={() => setPicked((p) => p.filter((x) => x !== id))} className="inline-flex h-8 items-center gap-1.5 rounded-full bg-primary/10 pl-1 pr-2.5 text-sm font-medium text-primary hover:bg-primary/15"><Avatar name={label} seed={id} size="size-6" className="text-[10px]" />{label}<X className="size-3.5" /></button>;
                  })}
                </div>
              )}
            </div>
            <ul className="no-scrollbar mt-2 max-h-64 overflow-y-auto px-3">
              {members.isLoading && <li className="flex items-center gap-2 px-2 py-4 text-sm text-muted-foreground"><Spinner /> Loading colleagues…</li>}
              {members.error && <li role="alert" className="px-2 py-4 text-sm text-destructive">{problemOf(members.error)}</li>}
              {!members.isLoading && !members.error && colleagues.length === 0 && <li className="px-2 py-6 text-center text-sm text-muted-foreground">{peopleSearch ? "No one matches." : "No colleagues to add yet."}</li>}
              {colleagues.map((c) => {
                const on = picked.includes(c.userId);
                const label = colleagueLabel(c);
                return (
                  <li key={c.userId}>
                    <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/60">
                      <input type="checkbox" className="sr-only" checked={on} onChange={(e) => setPicked((p) => (e.target.checked ? [...p, c.userId] : p.filter((x) => x !== c.userId)))} />
                      <Avatar name={label} seed={c.userId} size="size-9" />
                      <span className="min-w-0 flex-1 truncate text-sm font-medium">{label}</span>
                      <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                    </label>
                  </li>
                );
              })}
            </ul>
            <div className="space-y-2 border-t border-border px-5 py-4">
              {create.error && <p role="alert" className="text-sm text-destructive">{problemOf(create.error)}</p>}
              <button type="submit" disabled={create.isPending} className="flex h-11 w-full items-center justify-center gap-2 rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
                {create.isPending ? <Spinner /> : null}{picked.length ? `Create group with ${picked.length}` : "Create group"}
              </button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  );
}
