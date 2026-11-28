"use client";

/**
 * Status privacy (WhatsApp "Status privacy"): the default audience of new statuses, Allow
 * sharing, Close friends and named lists. All of it is stored by Messaging (ADR 0069), so it
 * is the same on every device; changes apply to statuses posted afterwards (earlier ones keep
 * their audience snapshot).
 */
import { useState } from "react";
import Link from "next/link";
import { ArrowLeft, ChevronRight, ListPlus, Pencil, Plus, Star, Trash2, Users } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "@/components/ui/toast";
import { socialKeys, useAudienceLists, useAudiencePreference, useCloseFriends, useEligibleContacts } from "@/hooks/v2/useSocial";
import { socialService, type AudienceList } from "@/services/v2/social.service";
import { audienceShortLabel, MODE_TITLES, selectionToChoice, type AudienceSelection } from "@/lib/status-privacy";
import { AudiencePicker, Overlay, PeopleChecklist, Toggle } from "./audience";
import { errorText } from "./shared";

export default function StatusPrivacy() {
  const client = useQueryClient();
  const pref = useAudiencePreference();
  const lists = useAudienceLists();
  const closeFriends = useCloseFriends();
  const eligible = useEligibleContacts();
  const [picking, setPicking] = useState(false);
  const [editingFriends, setEditingFriends] = useState(false);
  const [editingList, setEditingList] = useState<AudienceList | "new" | null>(null);
  const [busy, setBusy] = useState(false);
  const named = lists.data ?? [];
  const eligibleIds = new Set((eligible.data ?? []).map((p) => p.principalId ?? p.userId));
  const current: AudienceSelection | null = pref.data ? { mode: pref.data.mode, principalIds: pref.data.principalIds, listId: pref.data.listId } : null;

  const saveDefault = async (sel: AudienceSelection, allowSharing: boolean) => {
    setBusy(true);
    try {
      const saved = await socialService.putAudience({ ...selectionToChoice(sel), allowSharing });
      client.setQueryData(socialKeys.audience, saved);
      toast("Status privacy saved");
    } catch (e) {
      toast(errorText(e, "Status privacy couldn't be saved."), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto max-w-2xl px-4 pb-28 pt-6 sm:px-6 lg:py-8" data-testid="status-privacy">
      <header className="flex items-center gap-2">
        <Link href="/dashboard/updates" aria-label="Back to Updates" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <h1 className="text-2xl font-bold tracking-tight">Status privacy</h1>
      </header>

      <section className="mt-6">
        <h2 className="px-1 text-sm font-semibold text-muted-foreground">Who can see my status updates</h2>
        <div className="mt-2 rounded-2xl bg-card ring-1 ring-border">
          <button type="button" disabled={!pref.data || busy} onClick={() => setPicking(true)} data-testid="privacy-default"
            className="flex w-full items-center gap-3 rounded-2xl px-4 py-3.5 text-left hover:bg-muted/50 disabled:opacity-60">
            <Users className="size-5 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1">
              <span className="block font-semibold">{current ? MODE_TITLES[current.mode] : "My contacts"}</span>
              <span className="block truncate text-sm text-muted-foreground" data-testid="privacy-default-label">{pref.isPending ? "Loading…" : audienceShortLabel(current, named)}</span>
            </span>
            <ChevronRight className="size-5 text-muted-foreground" />
          </button>
        </div>
        <p className="mt-2 px-1 text-xs text-muted-foreground">Changes apply to status updates you post afterwards. Only mutual contacts can ever see your status. You can still choose a different audience for each status.</p>
        {pref.isError ? <p role="alert" className="mt-2 px-1 text-sm text-destructive">Status privacy couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void pref.refetch()}>Try again</button></p> : null}
      </section>

      <section className="mt-6">
        <div className="rounded-2xl bg-card ring-1 ring-border">
          <button type="button" role="switch" aria-checked={pref.data?.allowSharing ?? true} disabled={!pref.data || busy} data-testid="privacy-allow-sharing"
            onClick={() => current && pref.data && void saveDefault(current, !pref.data.allowSharing)}
            className="flex w-full items-center gap-3 rounded-2xl px-4 py-3.5 text-left disabled:opacity-60">
            <span className="min-w-0 flex-1">
              <span className="block font-semibold">Allow sharing</span>
              <span className="block text-sm text-muted-foreground">Viewers can forward or reshare your status updates, never to anyone outside the audience you chose.</span>
            </span>
            <Toggle on={pref.data?.allowSharing ?? true} />
          </button>
        </div>
      </section>

      <section className="mt-6">
        <h2 className="px-1 text-sm font-semibold text-muted-foreground">Close friends</h2>
        <div className="mt-2 rounded-2xl bg-card ring-1 ring-border">
          <button type="button" onClick={() => setEditingFriends(true)} data-testid="privacy-close-friends"
            className="flex w-full items-center gap-3 rounded-2xl px-4 py-3.5 text-left hover:bg-muted/50">
            <Star className="size-5 shrink-0 fill-emerald-500 text-emerald-500" />
            <span className="min-w-0 flex-1">
              <span className="block font-semibold">Close friends</span>
              <span className="block text-sm text-muted-foreground">{closeFriends.isPending ? "Loading…" : `${(closeFriends.data ?? []).filter((p) => eligibleIds.has(p)).length} people`}</span>
            </span>
            <ChevronRight className="size-5 text-muted-foreground" />
          </button>
        </div>
      </section>

      <section className="mt-6">
        <div className="flex items-center justify-between px-1">
          <h2 className="text-sm font-semibold text-muted-foreground">Custom lists</h2>
          <button type="button" onClick={() => setEditingList("new")} className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10" data-testid="privacy-new-list"><ListPlus className="size-4" />New list</button>
        </div>
        <ul className="mt-2 divide-y divide-border rounded-2xl bg-card ring-1 ring-border" data-testid="privacy-lists">
          {lists.isPending ? <li className="h-14 animate-pulse rounded-2xl bg-muted" /> : null}
          {!lists.isPending && !named.length ? <li className="px-4 py-4 text-sm text-muted-foreground">Make lists like “Family” or “Work” to share a status with them in one tap.</li> : null}
          {named.map((l) => (
            <li key={l.listId} className="flex items-center gap-2 px-2" data-testid="privacy-list">
              <button type="button" onClick={() => setEditingList(l)} className="flex min-w-0 flex-1 items-center gap-3 rounded-2xl px-2 py-3 text-left hover:bg-muted/50">
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-semibold">{l.name}</span>
                  <span className="block text-sm text-muted-foreground">{l.memberPrincipalIds.filter((p) => eligibleIds.has(p)).length} people</span>
                </span>
                <Pencil className="size-4 text-muted-foreground" aria-hidden />
              </button>
              <DeleteList list={l} isDefault={pref.data?.mode === "CUSTOM" && pref.data.listId === l.listId} />
            </li>
          ))}
        </ul>
      </section>

      {picking && pref.data ? (
        <AudiencePicker initial={current} allowSharing={pref.data.allowSharing} showSharing={false} title="Default status audience" doneLabel="Save"
          onClose={() => setPicking(false)} onDone={(sel) => { setPicking(false); void saveDefault(sel, pref.data!.allowSharing); }} />
      ) : null}
      {editingFriends ? <CloseFriendsEditor initial={closeFriends.data ?? []} onClose={() => setEditingFriends(false)} /> : null}
      {editingList ? <ListEditor list={editingList === "new" ? null : editingList} onClose={() => setEditingList(null)} /> : null}
    </div>
  );
}

function CloseFriendsEditor({ initial, onClose }: { initial: string[]; onClose: () => void }) {
  const client = useQueryClient();
  const eligible = useEligibleContacts();
  const [selected, setSelected] = useState(initial);
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      const saved = await socialService.setCloseFriends(selected);
      client.setQueryData(socialKeys.closeFriends, saved);
      toast("Close friends saved");
      onClose();
    } catch (e) {
      toast(errorText(e, "Close friends couldn't be saved."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Overlay title="Close friends" onClose={onClose} testId="close-friends-editor"
      footer={<button type="button" disabled={busy} onClick={() => void save()} className="h-11 w-full rounded-full bg-primary text-sm font-semibold text-primary-foreground disabled:opacity-50" data-testid="close-friends-save">{busy ? "Saving…" : "Save"}</button>}>
      <p className="mb-3 text-sm text-muted-foreground">Only you see this list. Changes apply to status updates you post afterwards.</p>
      <PeopleChecklist people={eligible.data ?? []} loading={eligible.isPending} selected={selected} onChange={setSelected} />
    </Overlay>
  );
}

function ListEditor({ list, onClose }: { list: AudienceList | null; onClose: () => void }) {
  const client = useQueryClient();
  const eligible = useEligibleContacts();
  const [name, setName] = useState(list?.name ?? "");
  const [selected, setSelected] = useState(list?.memberPrincipalIds ?? []);
  const [busy, setBusy] = useState(false);
  const save = async () => {
    if (!name.trim()) return;
    setBusy(true);
    try {
      if (list) await socialService.updateAudienceList(list.listId, { name: name.trim(), memberPrincipalIds: selected });
      else await socialService.createAudienceList(name.trim(), selected);
      await client.invalidateQueries({ queryKey: socialKeys.lists });
      toast(list ? "List saved" : "List created");
      onClose();
    } catch (e) {
      toast(errorText(e, "The list couldn't be saved."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Overlay title={list ? "Edit list" : "New list"} onClose={onClose} testId="list-editor"
      footer={<button type="button" disabled={busy || !name.trim()} onClick={() => void save()} className="h-11 w-full rounded-full bg-primary text-sm font-semibold text-primary-foreground disabled:opacity-50" data-testid="list-save">{busy ? "Saving…" : list ? "Save" : <span className="inline-flex items-center gap-1.5"><Plus className="size-4" />Create list</span>}</button>}>
      <label className="block">
        <span className="mb-1 block text-sm font-semibold">List name</span>
        <input autoFocus value={name} maxLength={80} onChange={(e) => setName(e.target.value)} placeholder="e.g. Family" className="h-12 w-full rounded-xl border border-border bg-background px-4 text-base focus:border-primary focus:outline-none" data-testid="list-name" />
      </label>
      <p className="mb-2 mt-4 text-sm font-semibold">People</p>
      <PeopleChecklist people={eligible.data ?? []} loading={eligible.isPending} selected={selected} onChange={setSelected} />
    </Overlay>
  );
}

function DeleteList({ list, isDefault }: { list: AudienceList; isDefault: boolean }) {
  const client = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const remove = async () => {
    setBusy(true);
    try {
      await socialService.deleteAudienceList(list.listId);
      await Promise.all([client.invalidateQueries({ queryKey: socialKeys.lists }), client.invalidateQueries({ queryKey: socialKeys.audience })]);
      toast("List deleted");
    } catch (e) {
      toast(errorText(e, "The list couldn't be deleted."), "error");
    } finally {
      setBusy(false);
      setConfirm(false);
    }
  };
  if (!confirm) {
    return <button type="button" aria-label={`Delete ${list.name}`} onClick={() => setConfirm(true)} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive" data-testid="list-delete"><Trash2 className="size-4" /></button>;
  }
  return (
    <span className="flex items-center gap-1">
      {isDefault ? <span className="hidden text-xs text-muted-foreground sm:inline">Your default becomes My contacts</span> : null}
      <button type="button" onClick={() => setConfirm(false)} className="h-8 rounded-full px-3 text-xs font-semibold hover:bg-muted">Cancel</button>
      <button type="button" disabled={busy} onClick={() => void remove()} className="h-8 rounded-full bg-destructive px-3 text-xs font-semibold text-white disabled:opacity-50" data-testid="list-delete-confirm">Delete</button>
    </span>
  );
}
