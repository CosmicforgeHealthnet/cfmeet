"use client";

/**
 * The chat list's own controls (WhatsApp): archive, pin and favourite a chat for yourself
 * (Chat info, next to the chat's notifications), your own chat lists ("Family", "Work"),
 * the filter chips above the list, and the Chats menu (⋮). Lists and favourites are kept by
 * Messaging for your account, so every device shows the same ones; nobody else sees them.
 */
import { useState } from "react";
import Link from "next/link";
import {
  Archive, ArchiveRestore, ArrowDown, ArrowLeft, ArrowUp, CalendarDays, Check, Heart, HeartOff, ListPlus, MessageSquarePlus, MoreVertical, Pencil, Pin, PinOff,
  Plus, Settings, Star, Timer, Trash2, Users,
} from "lucide-react";
import { chatsMenu, DEPLOYED } from "@/lib/section-menus";
import { Popover } from "radix-ui";
import type { ConversationView, MessagingEngine } from "@/lib/messaging/engine";
import { MAX_CHAT_LISTS, MAX_PINNED_CHATS, filterKey, sortLists, validListName, type ChatFilter, type ChatListView, type FilterChip } from "@/lib/messaging/chat-list";
import { MessagingApiError, messagingErrorMessage } from "@/lib/messaging/errors";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

const action = "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm font-medium hover:bg-muted disabled:opacity-50";

/** Archive / pin / favourite toggles and list membership for one chat (Chat info). */
export function ChatListToggles({ engine, conversation, pinnedTotal, lists }: { engine: MessagingEngine; conversation: ConversationView; pinnedTotal: number; lists?: ChatListView[] }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [listsOpen, setListsOpen] = useState(false);
  const change = (next: { archived?: boolean; pinned?: boolean; favourite?: boolean }) => {
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
      <button type="button" className={action} disabled={busy} data-testid="favourite-chat" onClick={() => change({ favourite: !conversation.favourite })}>
        {conversation.favourite ? <HeartOff className="size-4 text-muted-foreground" /> : <Heart className="size-4 text-muted-foreground" />}{conversation.favourite ? "Remove from Favourites" : "Add to Favourites"}
      </button>
      {lists ? (
        <button type="button" className={action} disabled={busy} data-testid="chat-in-lists" onClick={() => setListsOpen(true)}>
          <ListPlus className="size-4 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate">Add to list</span>
          {lists.some((l) => l.conversationIds.includes(conversation.id))
            ? <span className="truncate text-xs text-muted-foreground">{sortLists(lists).filter((l) => l.conversationIds.includes(conversation.id)).map((l) => l.name).join(", ")}</span>
            : null}
        </button>
      ) : null}
      {error ? <p role="alert" className="px-3 text-xs text-destructive">{error}</p> : null}
      {listsOpen && lists ? <ChatInListsDialog engine={engine} conversation={conversation} lists={lists} onClose={() => setListsOpen(false)} /> : null}
    </section>
  );
}

/** Which of my lists a chat is in (WhatsApp "Add to list"): tick to add, untick to remove. */
function ChatInListsDialog({ engine, conversation, lists, onClose }: { engine: MessagingEngine; conversation: ConversationView; lists: ChatListView[]; onClose: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const toggle = (l: ChatListView) => {
    setBusy(l.listId);
    setError(null);
    engine.setInChatList(l.listId, conversation.id, !l.conversationIds.includes(conversation.id))
      .catch((e) => setError(listError(e)))
      .finally(() => setBusy(null));
  };
  const create = () => {
    const valid = validListName(name);
    if (!valid) return setError("Give the list a name (up to 40 characters).");
    setBusy("new");
    setError(null);
    engine.createChatList(valid, [conversation.id])
      .then(() => setName(""), (e) => setError(listError(e)))
      .finally(() => setBusy(null));
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Add to list</DialogTitle>
          <DialogDescription>Only you see your lists. Taking a chat out of a list doesn&apos;t leave or change the chat.</DialogDescription>
        </DialogHeader>
        <ul className="-mx-1 max-h-72 overflow-y-auto" data-testid="chat-lists-membership">
          {sortLists(lists).map((l) => {
            const inList = l.conversationIds.includes(conversation.id);
            return (
              <li key={l.listId}>
                <button type="button" role="checkbox" aria-checked={inList} disabled={busy !== null} onClick={() => toggle(l)} className="flex w-full items-center gap-3 rounded-xl px-2 py-2.5 text-left text-[15px] hover:bg-muted disabled:opacity-60">
                  <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-md border", inList ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{inList ? <Check className="size-3.5" /> : null}</span>
                  <span className="min-w-0 flex-1 truncate">{l.name}</span>
                  <span className="text-xs text-muted-foreground">{l.conversationIds.length}</span>
                </button>
              </li>
            );
          })}
        </ul>
        {lists.length < MAX_CHAT_LISTS ? (
          <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); create(); }}>
            <input aria-label="New list name" placeholder="New list" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} className={field} />
            <button type="submit" disabled={busy !== null || !name.trim()} className="h-10 shrink-0 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground disabled:opacity-50">Create</button>
          </form>
        ) : null}
        {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
      </DialogContent>
    </Dialog>
  );
}

const field = "h-10 min-w-0 flex-1 rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary";

function listError(e: unknown): string {
  if (e instanceof MessagingApiError) {
    if (e.reason === "CHAT_LIST_NAME_TAKEN") return "You already have a list with that name.";
    if (e.reason === "CHAT_LIST_LIMIT_REACHED") return `You can have up to ${MAX_CHAT_LISTS} lists.`;
    if (e.reason === "CHAT_LIST_FULL") return "That list is full.";
  }
  return messagingErrorMessage(e);
}

/**
 * The filter chips above the chat list (WhatsApp): All, Unread, Favourites, Groups, your
 * lists — and "+" to make a new list. A chip shows its own chats; search stays inside it.
 */
export function FilterChips({ chips, active, onSelect, onCreateList, canCreate }: { chips: FilterChip[]; active: ChatFilter; onSelect: (f: ChatFilter) => void; onCreateList: () => void; canCreate: boolean }) {
  const current = filterKey(active);
  return (
    <div className="no-scrollbar -mx-4 flex gap-1.5 overflow-x-auto px-4" role="group" aria-label="Filter conversations" data-testid="chat-filters">
      {chips.map((chip) => (
        <button type="button" key={chip.key} aria-pressed={current === chip.key} onClick={() => onSelect(chip.filter)} data-testid={`filter-${chip.key.startsWith("list:") ? "list" : chip.key}`}
          className={cn("h-8 shrink-0 rounded-full px-3.5 text-sm font-medium transition-colors", current === chip.key ? "bg-primary/10 text-primary" : "bg-muted/60 text-muted-foreground hover:bg-muted hover:text-foreground")}>
          {chip.label}{chip.count ? <span className="ml-1 tabular-nums">{chip.count > 99 ? "99+" : chip.count}</span> : null}
        </button>
      ))}
      {canCreate ? (
        <button type="button" aria-label="New list" title="New list" onClick={onCreateList} data-testid="filter-new-list"
          className="flex size-8 shrink-0 items-center justify-center rounded-full bg-muted/60 text-muted-foreground hover:bg-muted hover:text-foreground">
          <Plus className="size-4" />
        </button>
      ) : null}
    </div>
  );
}

const menuItem = "flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-muted disabled:opacity-40";

const CHATS_MENU_ICONS: Record<string, typeof Users> = {
  "new-group": Users, "new-chat": MessageSquarePlus, lists: ListPlus, starred: Star, events: CalendarDays, settings: Settings,
};

/**
 * The Chats menu (⋮, WhatsApp; ADR 0074 section menus): New group, New conversation, Custom
 * lists, Starred messages, Events (once that section ships) and Settings — only what this
 * person may do here (lib/section-menus.ts).
 */
export function ChatsMenu({ disabled, onNewGroup, onNewChat, onLists, onSettings }: {
  disabled: boolean; onNewGroup: () => void; onNewChat: () => void; onLists: () => void; onSettings: () => void;
}) {
  const [open, setOpen] = useState(false);
  const run: Record<string, () => void> = { "new-group": onNewGroup, "new-chat": onNewChat, lists: onLists, settings: onSettings };
  const items = chatsMenu({ messaging: !disabled, lists: !disabled }, DEPLOYED);
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button type="button" aria-label="Menu" title="Menu" data-testid="chats-menu" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreVertical className="size-5" /></button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content side="bottom" align="end" sideOffset={6} collisionPadding={12} className="z-50 w-56 rounded-xl border border-border bg-popover p-1.5 shadow-xl" data-testid="chats-menu-items">
          {items.map(({ key, label, href }) => {
            const Icon = CHATS_MENU_ICONS[key] ?? Settings;
            const body = <><Icon className="size-4 text-muted-foreground" />{label}</>;
            return href ? (
              <Link key={key} href={href} data-testid={`chats-menu-${key}`} className={menuItem} onClick={() => setOpen(false)}>{body}</Link>
            ) : (
              <button key={key} type="button" data-testid={`chats-menu-${key}`} className={menuItem} onClick={() => { setOpen(false); run[key]?.(); }}>{body}</button>
            );
          })}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/**
 * Your chat lists (WhatsApp "Lists"): create, rename, reorder and delete them, and choose
 * the chats in each. Deleting a list or taking a chat out of it never touches the chat.
 */
export function ChatListsDialog({ engine, lists, conversations, startCreating, onClose }: {
  engine: MessagingEngine; lists: ChatListView[]; conversations: ConversationView[]; startCreating?: boolean; onClose: () => void;
}) {
  const [editing, setEditing] = useState<string | "new" | null>(startCreating ? "new" : null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ordered = sortLists(lists);
  const run = (job: Promise<unknown>, after?: () => void) => {
    setBusy(true);
    setError(null);
    job.then(() => after?.(), (e) => setError(listError(e))).finally(() => setBusy(false));
  };
  const target = editing && editing !== "new" ? lists.find((l) => l.listId === editing) ?? null : null;
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md" data-testid="chat-lists-dialog">
        {editing ? (
          <ListEditor key={editing} list={target} conversations={conversations} busy={busy}
            onBack={() => setEditing(null)}
            onSave={(name, chosen) => {
              if (!target) return run(engine.createChatList(name, chosen), () => setEditing(null));
              // One change at a time: each answer is the list as it now stands.
              const jobs: (() => Promise<unknown>)[] = [];
              if (name !== target.name) jobs.push(() => engine.renameChatList(target.listId, name));
              for (const id of chosen) if (!target.conversationIds.includes(id)) jobs.push(() => engine.setInChatList(target.listId, id, true));
              for (const id of target.conversationIds) if (!chosen.includes(id)) jobs.push(() => engine.setInChatList(target.listId, id, false));
              run(jobs.reduce<Promise<unknown>>((p, job) => p.then(job), Promise.resolve()), () => setEditing(null));
            }} error={error} />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Lists</DialogTitle>
              <DialogDescription>Sort your chats into your own lists. They show as filters above your chats, on every device. Only you see them.</DialogDescription>
            </DialogHeader>
            <ul className="-mx-1 max-h-80 space-y-0.5 overflow-y-auto" data-testid="chat-lists-manage">
              {ordered.length ? ordered.map((l, i) => (
                <li key={l.listId} className="flex items-center gap-1 rounded-xl px-1 hover:bg-muted/60">
                  <button type="button" onClick={() => setEditing(l.listId)} className="flex min-w-0 flex-1 items-center gap-3 py-2.5 pl-1 text-left">
                    <span className="min-w-0 flex-1"><span className="block truncate text-[15px] font-medium">{l.name}</span><span className="block text-xs text-muted-foreground">{l.conversationIds.length === 1 ? "1 chat" : `${l.conversationIds.length} chats`}</span></span>
                  </button>
                  <button type="button" aria-label={`Move ${l.name} up`} disabled={busy || i === 0} onClick={() => run(engine.moveChatList(l.listId, i - 1))} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted disabled:opacity-30"><ArrowUp className="size-4" /></button>
                  <button type="button" aria-label={`Move ${l.name} down`} disabled={busy || i === ordered.length - 1} onClick={() => run(engine.moveChatList(l.listId, i + 1))} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted disabled:opacity-30"><ArrowDown className="size-4" /></button>
                  <button type="button" aria-label={`Edit ${l.name}`} disabled={busy} onClick={() => setEditing(l.listId)} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Pencil className="size-4" /></button>
                  <button type="button" aria-label={`Delete ${l.name}`} disabled={busy} onClick={() => { if (window.confirm(`Delete the list "${l.name}"? Its chats stay in your chat list.`)) run(engine.deleteChatList(l.listId)); }} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-destructive"><Trash2 className="size-4" /></button>
                </li>
              )) : <li className="px-3 py-6 text-center text-sm text-muted-foreground">No lists yet. Make one for family, work or anything else.</li>}
            </ul>
            {lists.length < MAX_CHAT_LISTS
              ? <button type="button" disabled={busy} onClick={() => setEditing("new")} data-testid="chat-lists-new" className="flex h-11 w-full items-center justify-center gap-2 rounded-xl bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50"><Plus className="size-4" />New list</button>
              : <p className="text-center text-xs text-muted-foreground">You have the most lists you can make ({MAX_CHAT_LISTS}).</p>}
            {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function ListEditor({ list, conversations, busy, onBack, onSave, error }: {
  list: ChatListView | null; conversations: ConversationView[]; busy: boolean; onBack: () => void; onSave: (name: string, chosen: string[]) => void; error: string | null;
}) {
  const [name, setName] = useState(list?.name ?? "");
  const [chosen, setChosen] = useState<string[]>(list?.conversationIds ?? []);
  const [q, setQ] = useState("");
  const needle = q.trim().toLocaleLowerCase();
  const shown = conversations.filter((c) => !needle || c.title.toLocaleLowerCase().includes(needle));
  const valid = validListName(name);
  return (
    <>
      <DialogHeader>
        <div className="flex items-center gap-2">
          <button type="button" aria-label="Back to lists" onClick={onBack} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ArrowLeft className="size-4" /></button>
          <DialogTitle>{list ? "Edit list" : "New list"}</DialogTitle>
        </div>
        <DialogDescription className="sr-only">Name the list and choose its chats.</DialogDescription>
      </DialogHeader>
      <input aria-label="List name" placeholder="List name, e.g. Family" maxLength={40} value={name} onChange={(e) => setName(e.target.value)} className={cn(field, "w-full flex-none")} data-testid="chat-list-name" />
      <input type="search" aria-label="Search chats" placeholder="Search chats" value={q} onChange={(e) => setQ(e.target.value)} className={cn(field, "w-full flex-none")} />
      <ul className="-mx-1 max-h-64 overflow-y-auto" data-testid="chat-list-chats">
        {shown.map((c) => {
          const on = chosen.includes(c.id);
          return (
            <li key={c.id}>
              <button type="button" role="checkbox" aria-checked={on} onClick={() => setChosen(on ? chosen.filter((x) => x !== c.id) : [...chosen, c.id])} className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left text-[15px] hover:bg-muted">
                <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-md border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on ? <Check className="size-3.5" /> : null}</span>
                <span className="min-w-0 flex-1 truncate">{c.title}</span>
              </button>
            </li>
          );
        })}
        {!shown.length ? <li className="px-3 py-4 text-center text-sm text-muted-foreground">No chats match.</li> : null}
      </ul>
      {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
      <button type="button" disabled={busy || !valid} onClick={() => valid && onSave(valid, chosen)} data-testid="chat-list-save" className="h-11 w-full rounded-xl bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
        {list ? "Save" : chosen.length ? `Create with ${chosen.length === 1 ? "1 chat" : `${chosen.length} chats`}` : "Create list"}
      </button>
    </>
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
