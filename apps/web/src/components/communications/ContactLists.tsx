"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ListPlus, Pencil, Trash2, UserMinus, Users } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { SidePage, SideRow, SideSection } from "@/components/ui/side-page";
import { useHubContacts } from "@/hooks/v2/useCommunications";
import { communicationsService as hub, type ContactList } from "@/services/v2/communications.service";
import { communicationsErrorMessage } from "@/lib/communications/errors";

const LISTS_KEY = ["contact-lists"] as const;
const listKey = (id: string) => ["contact-lists", id] as const;
const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (k: string) => COLORS[([...k].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (n: string) => n.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
const field = "h-11 w-full rounded-xl border border-transparent bg-muted/70 px-3.5 text-sm outline-none focus:border-primary focus:bg-background focus:ring-2 focus:ring-primary/20";
const primaryBtn = "inline-flex h-10 shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50";

/** Contacts → Lists: your own groups of contacts, like "Family" or "Work". Only you see them. */
export default function ContactLists() {
  const lists = useQuery({ queryKey: LISTS_KEY, queryFn: () => hub.contactLists(), retry: false });
  const [creating, setCreating] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const items = lists.data?.data ?? [];
  const open = items.find((l) => l.listId === openId) ?? null;

  return (
    <div className="space-y-3">
      <div className="flex flex-col gap-3 px-1 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm text-muted-foreground">Group your contacts, like Family or Work. Only you see your lists.</p>
        <button type="button" className={`${primaryBtn} w-full sm:w-auto`} onClick={() => setCreating(true)}><ListPlus className="size-4" />New list</button>
      </div>
      {lists.isLoading ? [0, 1].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />) : null}
      {lists.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(lists.error)}</p> : null}
      {lists.data && items.length === 0 ? (
        <div className="flex flex-col items-center px-6 py-12 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Users className="size-6" /></span>
          <p className="mt-4 font-semibold">No lists yet</p>
          <p className="mt-1 max-w-xs text-sm text-muted-foreground">Make a list to find groups of people faster.</p>
        </div>
      ) : null}
      <ul className="space-y-1">
        {items.map((l) => (
          <li key={l.listId}>
            <button type="button" onClick={() => setOpenId(l.listId)} className="flex w-full items-center gap-3 rounded-2xl px-2 py-2.5 text-left hover:bg-muted/60">
              <span className="flex size-11 shrink-0 items-center justify-center rounded-xl text-sm font-semibold text-white" style={{ backgroundColor: colorFor(l.listId) }}>{initials(l.name)}</span>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-semibold">{l.name}</span>
                <span className="block text-sm text-muted-foreground">{l.memberCount} {l.memberCount === 1 ? "person" : "people"}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
      <ListForm open={creating} onOpenChange={setCreating} />
      {open ? <ListPage list={open} onClose={() => setOpenId(null)} /> : null}
    </div>
  );
}

function ListForm({ list, open, onOpenChange }: { list?: ContactList; open: boolean; onOpenChange: (v: boolean) => void }) {
  const client = useQueryClient();
  const [name, setName] = useState(list?.name ?? "");
  const save = useMutation({
    mutationFn: () => list ? hub.renameContactList(list.listId, name.trim(), list.version) : hub.createContactList(name.trim()),
    onSuccess: () => { void client.invalidateQueries({ queryKey: LISTS_KEY }); onOpenChange(false); if (!list) setName(""); },
  });
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader><DialogTitle>{list ? "Rename list" : "New list"}</DialogTitle><DialogDescription>{list ? "Give the list a new name." : "You can add people right after."}</DialogDescription></DialogHeader>
        <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); if (name.trim()) save.mutate(); }}>
          <input autoFocus aria-label="List name" className={field} value={name} maxLength={80} onChange={(e) => setName(e.target.value)} placeholder="e.g. Family" />
          {save.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(save.error)}</p> : null}
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => onOpenChange(false)} className="h-10 rounded-xl px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="submit" disabled={!name.trim() || save.isPending} className={primaryBtn}>{list ? "Save" : "Create list"}</button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ListPage({ list, onClose }: { list: ContactList; onClose: () => void }) {
  const client = useQueryClient();
  const detail = useQuery({ queryKey: listKey(list.listId), queryFn: () => hub.contactList(list.listId), retry: false });
  const contacts = useHubContacts();
  const [adding, setAdding] = useState("");
  const [renaming, setRenaming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const refresh = () => { void client.invalidateQueries({ queryKey: listKey(list.listId) }); void client.invalidateQueries({ queryKey: LISTS_KEY }); };
  const add = useMutation({ mutationFn: (contactId: string) => hub.setContactListMember(list.listId, contactId, true), onSuccess: () => { setAdding(""); refresh(); } });
  const remove = useMutation({ mutationFn: (contactId: string) => hub.setContactListMember(list.listId, contactId, false), onSuccess: refresh });
  const del = useMutation({ mutationFn: () => hub.deleteContactList(list.listId, (detail.data ?? list).version), onSuccess: () => { void client.invalidateQueries({ queryKey: LISTS_KEY }); onClose(); } });

  const members = detail.data?.members ?? [];
  const inList = new Set(members.map((m) => m.contactId));
  const candidates = (contacts.data ?? []).filter((c) => c.relationship === "CONNECTED" && !inList.has(c.contactId));
  const error = add.error ?? remove.error ?? del.error;

  return (
    <SidePage open onOpenChange={(v) => !v && onClose()} title={list.name} description="Contact list">
      <div className="flex flex-col items-center px-6 pb-5 pt-7 text-center">
        <span className="flex size-20 items-center justify-center rounded-3xl text-2xl font-semibold text-white" style={{ backgroundColor: colorFor(list.listId) }}>{initials(list.name)}</span>
        <p className="mt-3 text-xl font-semibold">{list.name}</p>
        <p className="text-xs text-muted-foreground">{members.length} {members.length === 1 ? "person" : "people"}</p>
      </div>
      {error ? <p role="alert" className="mx-4 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{communicationsErrorMessage(error)}</p> : null}
      <SideSection title="Add people">
        <div className="flex items-center gap-2 p-3">
          <NiceSelect className="min-w-0 flex-1" value={adding} onChange={setAdding} placeholder={candidates.length ? "Choose a contact" : "All your contacts are in this list"}
            options={candidates.map((c) => ({ value: c.contactId, label: c.alias || c.displayName }))} />
          <button type="button" disabled={!adding || add.isPending} onClick={() => add.mutate(adding)} className={primaryBtn}>Add</button>
        </div>
      </SideSection>
      <SideSection title="People">
        {detail.isLoading ? <div className="h-16 animate-pulse bg-muted" /> : null}
        {detail.data && members.length === 0 ? <p className="p-4 text-sm text-muted-foreground">No one in this list yet.</p> : null}
        {members.map((c) => {
          const name = c.alias || c.displayName;
          return (
            <div key={c.contactId} className="flex items-center gap-3 border-t border-border px-3 py-2.5 first:border-t-0">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white" style={{ backgroundColor: colorFor(c.cfMeetId) }}>{initials(name)}</span>
              <span className="min-w-0 flex-1 truncate text-[15px]">{name}</span>
              <button type="button" aria-label={`Remove ${name} from the list`} title="Remove from list" disabled={remove.isPending} onClick={() => remove.mutate(c.contactId)} className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive"><UserMinus className="size-4" /></button>
            </div>
          );
        })}
      </SideSection>
      <div className="pb-8">
        <SideSection>
          <SideRow icon={<Pencil />} label="Rename list" onClick={() => setRenaming(true)} />
          <SideRow icon={<Trash2 />} label="Delete list" hint="Your contacts stay; only the list goes" danger onClick={() => setDeleting(true)} />
        </SideSection>
      </div>
      <ListForm list={detail.data ?? list} open={renaming} onOpenChange={(v) => { setRenaming(v); if (!v) refresh(); }} />
      <Dialog open={deleting} onOpenChange={setDeleting}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader><DialogTitle>Delete {list.name}?</DialogTitle><DialogDescription>Your contacts stay. Only the list is removed.</DialogDescription></DialogHeader>
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setDeleting(false)} className="h-10 rounded-xl px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" disabled={del.isPending} onClick={() => del.mutate()} className="h-10 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-50">Delete</button>
          </div>
        </DialogContent>
      </Dialog>
    </SidePage>
  );
}
