"use client";

/**
 * Status audience (ADR 0069): the button beside Publish in every status editor, the picker it
 * opens, and the searchable people checklist shared with the Status privacy screen.
 *
 * The picker is an overlay above the editor, so opening it never loses the draft or the edits.
 * The choice applies to this status only (every split segment of it); the saved default and
 * Close Friends / named lists live on the server (the same on every device). Who may finally
 * see a status is decided by the server at publish time (snapshot of mutual, unblocked contacts).
 */
import { useMemo, useState } from "react";
import { Check, ChevronDown, ChevronRight, Lock, Search, Star, Users, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useAudienceLists, useAudiencePreference, useCloseFriends, useEligibleContacts } from "@/hooks/v2/useSocial";
import type { PersonView } from "@/services/v2/social.service";
import {
  audienceButtonLabel, audienceMembers, audienceShortLabel, deselectAll, normalizeSelection, searchPeople, selectAll, selectionToChoice, toggle,
  type AudienceMode, type AudienceSelection, type NamedList,
} from "@/lib/status-privacy";
import type { StatusJobInput } from "@/lib/status-publisher/types";
import { PersonAvatar } from "./shared";

export interface StatusAudienceState {
  /** null: the saved default. */
  selection: AudienceSelection | null;
  /** null: the saved default. */
  allowSharing: boolean | null;
}

/** The editor's audience: starts at the saved default; `jobOptions()` goes into enqueueStatus. */
export function useStatusAudience() {
  const pref = useAudiencePreference();
  const lists = useAudienceLists();
  const eligible = useEligibleContacts();
  const closeFriends = useCloseFriends();
  const [state, setState] = useState<StatusAudienceState>({ selection: null, allowSharing: null });
  const saved: AudienceSelection | null = pref.data ? { mode: pref.data.mode, principalIds: pref.data.principalIds, listId: pref.data.listId } : null;
  const effective = state.selection ?? saved;
  const named: NamedList[] = lists.data ?? [];
  const allowSharing = state.allowSharing ?? pref.data?.allowSharing ?? true;
  const label = audienceButtonLabel(effective, named);
  /** Options for enqueueStatus: an explicit choice, or nothing (the server applies the saved default). */
  const jobOptions = (): Pick<StatusJobInput, "audience" | "allowSharing" | "audienceLabel"> => ({
    ...(state.selection ? { audience: selectionToChoice(state.selection) } : {}),
    ...(state.allowSharing !== null ? { allowSharing: state.allowSharing } : {}),
    audienceLabel: audienceShortLabel(effective, named),
  });
  const people = eligible.data ?? [];
  const ids = people.map((p) => p.principalId ?? p.userId);
  /** Who the current choice reaches right now (the @mention picker offers only them). */
  const members = effective ? audienceMembers(effective, ids, closeFriends.data ?? [], named) : ids;
  const memberSet = new Set(members);
  const mentionable = people.filter((p) => memberSet.has(p.principalId ?? p.userId));
  return { state, setState, effective, allowSharing, label, lists: named, jobOptions, members, mentionable };
}

export type StatusAudienceHandle = ReturnType<typeof useStatusAudience>;

/** The pill beside Publish; opens the picker over the editor. */
export function AudienceButton({ audience, className, tone = "dark", showSharing = true }: { audience: StatusAudienceHandle; className?: string; tone?: "dark" | "light"; showSharing?: boolean }) {
  const [open, setOpen] = useState(false);
  const Icon = audience.effective?.mode === "CLOSE_FRIENDS" ? Star : audience.effective?.mode === "CONTACTS" || !audience.effective ? Users : Lock;
  return (
    <>
      <button type="button" onClick={() => setOpen(true)} data-testid="status-audience" aria-haspopup="dialog"
        className={cn("flex h-9 max-w-full items-center gap-1.5 rounded-full px-3 text-sm font-semibold",
          tone === "dark" ? "bg-white/15 text-white hover:bg-white/25" : "bg-muted text-foreground hover:bg-muted/80", className)}>
        <Icon className={cn("size-4 shrink-0", audience.effective?.mode === "CLOSE_FRIENDS" && "fill-emerald-500 text-emerald-500")} />
        <span className="truncate">{audience.label}</span>
        <ChevronDown className="size-4 shrink-0 opacity-70" />
      </button>
      {open ? (
        <AudiencePicker initial={audience.effective} allowSharing={audience.allowSharing} showSharing={showSharing}
          onClose={() => setOpen(false)}
          onDone={(selection, allowSharing) => { audience.setState({ selection, allowSharing }); setOpen(false); }} />
      ) : null}
    </>
  );
}

/** Full-screen on phones, a dialog on computers; sits above the editors (z-[90]). */
export function Overlay({ title, onClose, children, footer, testId }: { title: string; onClose: () => void; children: React.ReactNode; footer?: React.ReactNode; testId?: string }) {
  return (
    <div className="fixed inset-0 z-[90] flex items-end justify-center bg-black/50 sm:items-center" onClick={onClose}>
      <div role="dialog" aria-label={title} data-testid={testId} onClick={(e) => e.stopPropagation()}
        className="flex h-[92vh] w-full max-w-lg flex-col rounded-t-3xl bg-background text-foreground shadow-2xl sm:h-auto sm:max-h-[88vh] sm:rounded-3xl">
        <div className="flex items-center justify-between gap-2 px-4 pb-2 pt-4">
          <h2 className="truncate text-lg font-bold">{title}</h2>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-3">{children}</div>
        {footer ? <div className="border-t border-border p-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">{footer}</div> : null}
      </div>
    </div>
  );
}

const MODES: { mode: AudienceMode; title: string; hint: string }[] = [
  { mode: "CONTACTS", title: "My contacts", hint: "Your mutual contacts" },
  { mode: "CONTACTS_EXCEPT", title: "My contacts except…", hint: "Leave some contacts out" },
  { mode: "ONLY", title: "Only share with…", hint: "Choose who sees it" },
  { mode: "CLOSE_FRIENDS", title: "Close friends", hint: "Your Close friends list" },
];

/**
 * The audience picker: modes, named lists, a searchable list of eligible contacts with select
 * all / deselect all and counts, and Allow sharing. Nothing changes until Done.
 */
export function AudiencePicker({ initial, allowSharing: initialSharing, onDone, onClose, title = "Who can see this status", doneLabel = "Done", showSharing = true }: {
  initial: AudienceSelection | null;
  allowSharing: boolean;
  onDone: (selection: AudienceSelection, allowSharing: boolean) => void;
  onClose: () => void;
  title?: string;
  doneLabel?: string;
  showSharing?: boolean;
}) {
  const eligible = useEligibleContacts();
  const closeFriends = useCloseFriends();
  const lists = useAudienceLists();
  const [mode, setMode] = useState<AudienceMode>(initial?.mode ?? "CONTACTS");
  const [except, setExcept] = useState<string[]>(initial?.mode === "CONTACTS_EXCEPT" ? initial.principalIds ?? [] : []);
  const [only, setOnly] = useState<string[]>(initial?.mode === "ONLY" ? initial.principalIds ?? [] : []);
  const [listId, setListId] = useState<string | null>(initial?.mode === "CUSTOM" ? initial.listId ?? null : null);
  const [sharing, setSharing] = useState(initialSharing);
  const people = eligible.data ?? [];
  const ids = people.map((p) => p.principalId ?? p.userId);
  const named = lists.data ?? [];
  const selection: AudienceSelection = mode === "CONTACTS_EXCEPT" ? { mode, principalIds: except } : mode === "ONLY" ? { mode, principalIds: only } : mode === "CUSTOM" ? { mode, listId } : { mode };
  const normalized = normalizeSelection(selection, ids, named);
  const reach = normalized ? audienceMembers(normalized, ids, closeFriends.data ?? [], named).length : 0;

  return (
    <Overlay title={title} onClose={onClose} testId="audience-picker"
      footer={(
        <div className="flex items-center gap-3">
          <p className="min-w-0 flex-1 text-sm text-muted-foreground" data-testid="audience-reach">
            {eligible.isPending ? "Loading contacts…" : normalized ? `${reach} of ${ids.length} ${ids.length === 1 ? "contact" : "contacts"} can see it` : mode === "ONLY" ? "Choose at least one person" : "Choose a list"}
          </p>
          <button type="button" disabled={!normalized} onClick={() => normalized && onDone(normalized, sharing)} data-testid="audience-done"
            className="h-11 shrink-0 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground disabled:opacity-50">{doneLabel}</button>
        </div>
      )}>
      {eligible.isError ? <p role="alert" className="mb-2 text-sm text-destructive">Your contacts couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void eligible.refetch()}>Try again</button></p> : null}
      <ul className="space-y-1" role="radiogroup" aria-label="Audience">
        {MODES.map((m) => {
          const count = m.mode === "CONTACTS_EXCEPT" ? `${except.length} excluded` : m.mode === "ONLY" ? `${only.length} selected` : m.mode === "CLOSE_FRIENDS" ? `${(closeFriends.data ?? []).filter((p) => ids.includes(p)).length} people` : `${ids.length} people`;
          return (
            <li key={m.mode}>
              <ModeRow checked={mode === m.mode} title={m.title} hint={m.hint} count={count} onClick={() => setMode(m.mode)} testId={`audience-mode-${m.mode}`} />
            </li>
          );
        })}
        {named.map((l) => (
          <li key={l.listId}>
            <ModeRow checked={mode === "CUSTOM" && listId === l.listId} title={l.name} hint="Custom list" count={`${l.memberPrincipalIds.filter((p) => ids.includes(p)).length} people`}
              onClick={() => { setMode("CUSTOM"); setListId(l.listId); }} testId="audience-mode-CUSTOM" />
          </li>
        ))}
      </ul>
      {mode === "CLOSE_FRIENDS" && !(closeFriends.data ?? []).length ? (
        <p className="mt-2 rounded-xl bg-muted px-3 py-2 text-sm text-muted-foreground">Your Close friends list is empty. Add people in Status privacy.</p>
      ) : null}

      {mode === "CONTACTS_EXCEPT" || mode === "ONLY" ? (
        <div className="mt-4">
          <p className="mb-2 text-sm font-semibold">{mode === "ONLY" ? "Share only with" : "Hide from"}</p>
          <PeopleChecklist people={people} loading={eligible.isPending} selected={mode === "ONLY" ? only : except} onChange={mode === "ONLY" ? setOnly : setExcept} />
        </div>
      ) : null}

      {showSharing ? (
        <button type="button" role="switch" aria-checked={sharing} onClick={() => setSharing(!sharing)} data-testid="audience-allow-sharing"
          className="mt-4 flex w-full items-center gap-3 rounded-2xl px-1 py-2 text-left">
          <span className="min-w-0 flex-1">
            <span className="block text-sm font-semibold">Allow sharing</span>
            <span className="block text-xs text-muted-foreground">Viewers can forward or reshare it, never beyond the people you chose.</span>
          </span>
          <Toggle on={sharing} />
        </button>
      ) : null}
    </Overlay>
  );
}

function ModeRow({ checked, title, hint, count, onClick, testId }: { checked: boolean; title: string; hint: string; count: string; onClick: () => void; testId: string }) {
  return (
    <button type="button" role="radio" aria-checked={checked} onClick={onClick} data-testid={testId}
      className={cn("flex w-full items-center gap-3 rounded-2xl px-2 py-2.5 text-left hover:bg-muted/70", checked && "bg-primary/5")}>
      <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border-2", checked ? "border-primary bg-primary" : "border-muted-foreground/40")}>
        {checked ? <span className="size-2 rounded-full bg-primary-foreground" /> : null}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[15px] font-semibold">{title}</span>
        <span className="block truncate text-xs text-muted-foreground">{hint}</span>
      </span>
      <span className="shrink-0 text-xs text-muted-foreground">{count}</span>
      {checked ? <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden /> : null}
    </button>
  );
}

export function Toggle({ on }: { on: boolean }) {
  return (
    <span aria-hidden className={cn("relative inline-flex h-6 w-11 shrink-0 rounded-full transition-colors", on ? "bg-primary" : "bg-muted-foreground/30")}>
      <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-transform", on ? "translate-x-[22px]" : "translate-x-0.5")} />
    </span>
  );
}

/** Searchable checklist of people with avatars, select all / deselect all (of the shown ones) and counts. */
export function PeopleChecklist({ people, selected, onChange, loading }: { people: PersonView[]; selected: string[]; onChange: (ids: string[]) => void; loading?: boolean }) {
  const [q, setQ] = useState("");
  const shown = useMemo(() => searchPeople(people, q), [people, q]);
  const shownIds = shown.map((p) => p.principalId ?? p.userId);
  const allShown = shownIds.length > 0 && shownIds.every((id) => selected.includes(id));
  return (
    <div data-testid="people-checklist">
      <label className="flex h-11 items-center gap-2 rounded-full bg-muted px-4">
        <Search className="size-4 text-muted-foreground" />
        <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search contacts" aria-label="Search contacts" className="min-w-0 flex-1 bg-transparent text-sm focus:outline-none" data-testid="people-search" />
      </label>
      <div className="mt-2 flex items-center justify-between gap-2 px-1 text-sm">
        <span className="text-muted-foreground" data-testid="people-count">{selected.length} of {people.length} selected</span>
        <span className="flex gap-1">
          <button type="button" disabled={!shownIds.length || allShown} onClick={() => onChange(selectAll(selected, shownIds))} className="rounded-full px-3 py-1 font-semibold text-primary hover:bg-primary/10 disabled:opacity-40" data-testid="people-select-all">Select all</button>
          <button type="button" disabled={!shownIds.some((id) => selected.includes(id))} onClick={() => onChange(deselectAll(selected, shownIds))} className="rounded-full px-3 py-1 font-semibold text-primary hover:bg-primary/10 disabled:opacity-40" data-testid="people-deselect-all">Deselect all</button>
        </span>
      </div>
      <ul className="mt-1">
        {loading ? [0, 1, 2].map((i) => <li key={i} className="my-2 h-12 animate-pulse rounded-xl bg-muted" />) : null}
        {!loading && !people.length ? <li className="py-6 text-center text-sm text-muted-foreground">No contacts yet. Statuses reach mutual contacts only.</li> : null}
        {!loading && people.length && !shown.length ? <li className="py-6 text-center text-sm text-muted-foreground">No one matches “{q}”</li> : null}
        {shown.map((p) => {
          const id = p.principalId ?? p.userId;
          const on = selected.includes(id);
          return (
            <li key={id}>
              <button type="button" role="checkbox" aria-checked={on} onClick={() => onChange(toggle(selected, id))} data-testid="people-item"
                className="flex w-full items-center gap-3 rounded-2xl px-1 py-2 text-left hover:bg-muted/60">
                <PersonAvatar person={p} size="size-10" />
                <span className="min-w-0 flex-1 truncate font-medium">{p.name}</span>
                <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-full border-2", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>
                  {on ? <Check className="size-4" strokeWidth={3} /> : null}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
