"use client";

/**
 * AddPeopleDialog — invite people into the ongoing call (C7), WhatsApp "Add participant" style:
 * a dark sheet in the call's colours, search, picked people as chips, and one Add button.
 *
 * People: your personal contacts (added by contact id; the server resolves them) and colleagues from the directory (resolved to Calling principals). POST
 * /v1/calls/{id}/participants goes through the adapter, which injects the signed-in user's own
 * active participant as the inviter; Calling resolves, policy-checks and rings each person.
 */
import { useMemo, useState } from "react";
import { Check, Search, UserPlus, X } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Dialog as DialogPrimitive } from "radix-ui";
import { Spinner } from "@/components/ui/spinner";
import { useCallingMutation } from "@/hooks/calling/useCalls";
import { identityApi } from "@/lib/messaging/api";
import { hubCallsService } from "@/services/calling/hub-calls.service";
import { callsService } from "@/services/calling/calls.service";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { cn } from "@/lib/utils";

/** People one request may add (Calling's group limit is 16 including the caller). */
const MAX_ADDED = 15;
const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (k: string) => COLORS[([...k].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (n: string) => n.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

type Person = { key: string; kind: "contact" | "colleague"; id: string; name: string; principalId?: string | null };

export default function AddPeopleDialog({ callId, version, principalId, inviterParticipantId, participantPrincipalIds, open, onOpenChange }: {
  callId: string;
  /** This user's own participant in the call (the server needs it as the inviter for contacts). */
  inviterParticipantId: string | null;
  version: number;
  principalId: string;
  /** Principals already in the call (never offered again). */
  participantPrincipalIds: string[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [filter, setFilter] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const mutation = useCallingMutation();
  const directory = useQuery({ queryKey: ["calling", "directory", principalId], queryFn: () => identityApi.directory(), enabled: open, retry: false });
  const contacts = useQuery({ queryKey: ["calling", "hub-contacts"], queryFn: () => hubCallsService.contacts(), enabled: open, retry: false, staleTime: 60_000 });
  const q = filter.trim().toLowerCase();
  // Personal contacts (WhatsApp) first, then organization colleagues.
  const people = useMemo<Person[]>(() => [
    ...(contacts.data?.data ?? [])
      .filter((c) => c.relationship === "CONNECTED" || c.relationship == null)
      .map((c) => ({ key: `c:${c.contactId}`, kind: "contact" as const, id: c.contactId, name: c.alias || c.displayName })),
    ...(directory.data?.people ?? [])
      .filter((p) => p.principalId !== principalId && !(p.principalId && participantPrincipalIds.includes(p.principalId)))
      .map((p) => ({ key: `u:${p.userId}`, kind: "colleague" as const, id: p.userId, name: p.displayName, principalId: p.principalId })),
  ], [contacts.data, directory.data, principalId, participantPrincipalIds]);
  const shown = people.filter((p) => p.name.toLowerCase().includes(q)).slice(0, 80);
  const chosen = people.filter((p) => picked.includes(p.key));
  const toggle = (key: string) => setPicked((current) => current.includes(key) ? current.filter((k) => k !== key) : current.length >= MAX_ADDED ? current : [...current, key]);
  const close = () => { onOpenChange(false); setPicked([]); setFilter(""); setError(null); };
  const add = async () => {
    setError(null);
    try {
      const pickedContacts = chosen.filter((p) => p.kind === "contact").map((p) => p.id);
      const colleagues = chosen.filter((p) => p.kind === "colleague");
      let currentVersion = version;
      if (pickedContacts.length) {
        if (!inviterParticipantId) throw new Error("You need to be on the call to add people.");
        await hubCallsService.addContactsToCall(callId, { targetContactIds: pickedContacts, invitingParticipantId: inviterParticipantId, expectedVersion: currentVersion });
        if (colleagues.length) currentVersion = (await callsService.get(callId)).version;
      }
      if (colleagues.length) {
        const missing = colleagues.filter((p) => !p.principalId).map((p) => p.id);
        const resolved = missing.length ? (await identityApi.resolve(missing)).people : [];
        const ids = colleagues.map((p) => p.principalId ?? resolved.find((r) => r.userId === p.id)?.principalId).filter((id): id is string => !!id);
        if (ids.length !== colleagues.length) throw new Error("Some of the people you chose can't receive calls yet.");
        await mutation.mutateAsync({ operation: "addParticipants", callId, key: newIdempotencyKey(), body: { targetPrincipalIds: ids, expectedVersion: currentVersion } });
      }
      close();
    } catch (e) { setError(callingErrorMessage(e)); }
  };

  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-[85] bg-black/60 backdrop-blur-sm" />
        <DialogPrimitive.Content className="dark fixed inset-x-0 bottom-0 z-[90] flex max-h-[88dvh] flex-col rounded-t-[28px] bg-[#1B1C1E] text-white shadow-2xl ring-1 ring-white/10 outline-none sm:inset-auto sm:left-1/2 sm:top-1/2 sm:max-h-[80vh] sm:w-[28rem] sm:-translate-x-1/2 sm:-translate-y-1/2 sm:rounded-[28px]">
          <div className="mx-auto mt-2.5 h-1 w-10 rounded-full bg-white/20 sm:hidden" aria-hidden />
          <header className="flex items-center gap-2 px-4 pb-3 pt-3 sm:pt-5">
            <div className="min-w-0 flex-1">
              <DialogPrimitive.Title className="text-lg font-semibold">Add people</DialogPrimitive.Title>
              <DialogPrimitive.Description className="text-xs text-white/60">{picked.length ? `${picked.length} selected` : "They'll be called and join when they answer."}</DialogPrimitive.Description>
            </div>
            <DialogPrimitive.Close className="flex size-10 items-center justify-center rounded-full text-white/70 hover:bg-white/10" aria-label="Close"><X className="size-5" /></DialogPrimitive.Close>
          </header>

          <div className="space-y-3 px-4">
            <label className="flex h-11 items-center gap-2 rounded-xl bg-white/10 px-3.5 text-white/60 focus-within:ring-2 focus-within:ring-[#8AB4FF]">
              <Search className="size-4 shrink-0" />
              <input value={filter} placeholder="Search name" onChange={(e) => setFilter(e.target.value)} aria-label="Search people"
                className="min-w-0 flex-1 bg-transparent text-sm text-white outline-none placeholder:text-white/50" />
            </label>
            {chosen.length > 0 ? (
              <div className="no-scrollbar flex gap-3 overflow-x-auto pb-1">
                {chosen.map((p) => (
                  <button key={p.key} type="button" onClick={() => toggle(p.key)} aria-label={`Remove ${p.name}`} className="flex w-14 shrink-0 flex-col items-center gap-1">
                    <span className="relative flex size-12 items-center justify-center rounded-full text-sm font-semibold" style={{ backgroundColor: colorFor(p.key) }}>
                      {initials(p.name)}
                      <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-[#3C4043] ring-2 ring-[#1B1C1E]"><X className="size-3" /></span>
                    </span>
                    <span className="w-full truncate text-center text-[11px] text-white/80">{p.name.split(" ")[0]}</span>
                  </button>
                ))}
              </div>
            ) : null}
            {error ? <p role="alert" className="rounded-xl bg-red-500/15 px-3 py-2 text-sm text-red-200">{error}</p> : null}
          </div>

          <ul className="no-scrollbar mt-2 min-h-[12rem] flex-1 overflow-y-auto px-2 pb-2">
            {directory.isPending && contacts.isPending ? <li className="flex items-center gap-2 px-3 py-5 text-sm text-white/60"><Spinner /> Loading people…</li>
              : directory.error && contacts.error ? <li role="alert" className="px-3 py-5 text-sm text-red-200">{callingErrorMessage(directory.error)}</li>
                : shown.length === 0 ? <li className="px-3 py-10 text-center text-sm text-white/60">{q ? "No one matches." : "No one else to add."}</li> : null}
            {shown.map((p, i) => {
              const on = picked.includes(p.key);
              const heading = i === 0 || shown[i - 1].kind !== p.kind ? (p.kind === "contact" ? "Contacts" : "Colleagues") : null;
              return (
                <li key={p.key}>
                  {heading ? <p className="px-2 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-white/50">{heading}</p> : null}
                  <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-white/5">
                    <input type="checkbox" className="sr-only" checked={on} onChange={() => toggle(p.key)} />
                    <span className="flex size-11 shrink-0 items-center justify-center rounded-full text-sm font-semibold" style={{ backgroundColor: colorFor(p.key) }}>{initials(p.name)}</span>
                    <span className="min-w-0 flex-1 truncate text-[15px]">{p.name}</span>
                    <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-full border-2", on ? "border-[#8AB4FF] bg-[#8AB4FF] text-[#0A2A5E]" : "border-white/30")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                  </label>
                </li>
              );
            })}
          </ul>

          <footer className="border-t border-white/10 px-4 pb-[max(1rem,env(safe-area-inset-bottom))] pt-3">
            <button type="button" disabled={!picked.length || mutation.isPending} onClick={() => void add()}
              className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-[#1E8E3E] text-sm font-semibold text-white hover:bg-[#188038] disabled:opacity-40">
              {mutation.isPending ? <Spinner /> : <UserPlus className="size-5" />}{picked.length ? `Add ${picked.length} to call` : "Add to call"}
            </button>
          </footer>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
