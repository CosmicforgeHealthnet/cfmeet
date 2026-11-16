"use client";

/**
 * AddPeopleDialog — invite people into the ongoing call (C7).
 *
 * The same people list as the dialer's group call (the colleague directory, resolved to Calling
 * principals). POST /v1/calls/{id}/participants goes through the adapter, which injects the
 * signed-in user's own active participant as the inviter; Calling resolves, policy-checks and
 * rings each person on their devices, and the call becomes a group call.
 */

import { useMemo, useState } from "react";
import { Check, UserPlus } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useCallingMutation } from "@/hooks/calling/useCalls";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { cn } from "@/lib/utils";

/** People one request may add (Calling's group limit is 16 including the caller). */
const MAX_ADDED = 15;

export default function AddPeopleDialog({ callId, version, principalId, participantPrincipalIds, open, onOpenChange }: {
  callId: string;
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
  const q = filter.trim().toLowerCase();
  const people = useMemo(
    () => (directory.data?.people ?? []).filter((p) => p.principalId !== principalId && !(p.principalId && participantPrincipalIds.includes(p.principalId))),
    [directory.data, principalId, participantPrincipalIds],
  );
  const shown = people.filter((p) => p.displayName.toLowerCase().includes(q)).slice(0, 50);
  const toggle = (userId: string) => setPicked((current) => current.includes(userId) ? current.filter((id) => id !== userId) : current.length >= MAX_ADDED ? current : [...current, userId]);
  const close = () => { onOpenChange(false); setPicked([]); setFilter(""); setError(null); };

  const add = async () => {
    setError(null);
    try {
      const chosen = people.filter((p) => picked.includes(p.userId));
      const missing = chosen.filter((p) => !p.principalId).map((p) => p.userId);
      const resolved = missing.length ? (await identityApi.resolve(missing)).people : [];
      const ids = chosen.map((p) => p.principalId ?? resolved.find((r) => r.userId === p.userId)?.principalId).filter((id): id is string => !!id);
      if (!ids.length || ids.length !== chosen.length) throw new Error("Some of the people you chose can't receive calls yet.");
      await mutation.mutateAsync({ operation: "addParticipants", callId, key: newIdempotencyKey(), body: { targetPrincipalIds: ids, expectedVersion: version } });
      close();
    } catch (e) { setError(callingErrorMessage(e)); }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogContent className="z-[90] sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2"><UserPlus className="size-5" /> Add people</DialogTitle>
          <DialogDescription>Everyone you add is rung on their devices and joins this call when they answer.</DialogDescription>
        </DialogHeader>
        {error ? <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error}</p> : null}
        <input value={filter} placeholder="Search name" onChange={(e) => setFilter(e.target.value)} aria-label="Search people"
          className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm outline-none" />
        <ul className="max-h-72 overflow-y-auto">
          {directory.isPending ? <li className="flex items-center gap-2 px-2 py-4 text-sm text-muted-foreground"><Spinner /> Loading…</li>
            : directory.error ? <li role="alert" className="px-2 py-4 text-sm text-destructive">{callingErrorMessage(directory.error)}</li>
              : shown.length === 0 ? <li className="px-2 py-6 text-center text-sm text-muted-foreground">{q ? "No one matches." : "No one else to add."}</li> : null}
          {shown.map((p) => {
            const on = picked.includes(p.userId);
            return (
              <li key={p.userId}>
                <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/60">
                  <input type="checkbox" className="sr-only" checked={on} onChange={() => toggle(p.userId)} />
                  <span className="min-w-0 flex-1 truncate text-sm font-medium">{p.displayName}</span>
                  <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                </label>
              </li>
            );
          })}
        </ul>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={close}>Cancel</Button>
          <Button type="button" disabled={!picked.length || mutation.isPending} onClick={() => void add()}>
            {mutation.isPending ? <Spinner className="mr-2 size-4" /> : null} Add{picked.length ? ` (${picked.length})` : ""}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
