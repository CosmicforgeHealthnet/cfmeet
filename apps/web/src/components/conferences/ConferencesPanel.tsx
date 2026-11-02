"use client";

import { useState } from "react";
import Link from "next/link";
import type { ConferenceResponse, CreateConferenceRequest, MeetingSpaceResponse } from "@/types/v2";
import { useConferences, useCreateConference, usePrepareConference, useStartConference, useLockConference, useUnlockConference, useEndConference, useCancelConference, useRescheduleConference } from "@/hooks/v2/useConferences";
import { scheduledWindow, unstarted, joinable, conferencePermission } from "@/lib/meeting-scheduling";
import { Button } from "@/components/ui/button";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";

export default function ConferencesPanel({ space }: { space: MeetingSpaceResponse }) {
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const query = useConferences(space.id, { ...(from ? { from: new Date(`${from}T00:00:00`).toISOString() } : {}), ...(to ? { to: new Date(`${to}T23:59:59`).toISOString() } : {}) });
  const create = useCreateConference(space.id);
  const canManage = conferencePermission(space, "start");
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const scheduleOccurrence = (): CreateConferenceRequest => {
    if (!start) {
      if (end) throw new Error("Choose a start date before an end date.");
      return {};
    }
    if (!end) {
      const startAt = new Date(start);
      if (!Number.isFinite(+startAt) || +startAt <= Date.now()) {
        throw new Error("Choose a future start date and time.");
      }
      return { scheduledStartAt: startAt.toISOString() };
    }
    return scheduledWindow(start, end);
  };
  return <section id="conferences" className="space-y-4">
    <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-lg font-semibold">{space.title || space.meetingCode} — conferences</h2><p className="text-sm text-muted-foreground">Each session has its own lifecycle. Dates below use {timezone}.</p></div>
      {canManage && <ActionDialog label="Schedule occurrence" description={`Create one conference occurrence. Leave both dates blank for an unscheduled occurrence, or enter a start and optional end in ${timezone}.`} submit={async () => { await create.mutateAsync(scheduleOccurrence()); setStart(""); setEnd(""); }}><Field label="Start (optional)" type="datetime-local" value={start} onChange={e => setStart(e.target.value)} /><Field label="End (optional)" type="datetime-local" min={start || undefined} disabled={!start} value={end} onChange={e => setEnd(e.target.value)} /></ActionDialog>}
    </div>
    <div className="grid gap-3 sm:grid-cols-2"><Field label="From date" type="date" value={from} onChange={e => setFrom(e.target.value)} /><Field label="Through date" type="date" min={from} value={to} onChange={e => setTo(e.target.value)} /></div>
    {query.isLoading && <p role="status">Loading occurrences…</p>}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {!query.isError && query.data?.items.length === 0 && <p className="rounded-xl border border-dashed p-8 text-center text-muted-foreground">No occurrences in this date range.</p>}
    {query.data?.items.map(conference => <ConferenceRow key={conference.id} conference={conference} space={space} canManage={canManage} />)}
  </section>;
}

export function ConferenceRow({ conference: c, space, canManage }: { conference: ConferenceResponse; space: MeetingSpaceResponse; canManage: boolean }) {
  const prepare = usePrepareConference(), start = useStartConference(), lock = useLockConference(), unlock = useUnlockConference(), end = useEndConference(), cancel = useCancelConference(), reschedule = useRescheduleConference(c.id);
  const [newStart, setNewStart] = useState("");
  const [newEnd, setNewEnd] = useState("");
  const mutable = space.status === "ACTIVE" && c.view === "FULL" && !c.isLegacy;
  return <article className="space-y-3 rounded-xl border border-border bg-card p-4">
    <div className="flex flex-wrap items-center justify-between gap-2"><div><p className="font-medium">{c.scheduledStartAt ? new Date(c.scheduledStartAt).toLocaleString() : "Unscheduled conference"}</p><p className="text-xs text-muted-foreground break-all">{c.id} · {c.kind || "Conference"}{c.isException ? " · Exception" : ""}</p>{c.scheduledEndAt && <p className="text-sm text-muted-foreground">Until {new Date(c.scheduledEndAt).toLocaleString()}</p>}</div><span className="rounded-full bg-muted px-3 py-1 text-xs font-medium">{c.status}{c.isLocked ? " · Locked" : ""}</span></div>
    {c.status === "ENDING" && <p className="text-sm text-muted-foreground">Admission has stopped. Waiting for the media provider to confirm termination.</p>}
    <div className="flex flex-wrap gap-2">
      <Button size="sm" variant="outline" asChild><Link href={`/dashboard/meetings/${space.id}/conferences/${c.id}`}>Details &amp; participants</Link></Button>
      {space.status === "ACTIVE" && joinable(c.status) && <Button size="sm" asChild><Link href={`/${space.meetingCode}?conference=${encodeURIComponent(c.id)}`}>Join occurrence</Link></Button>}
      {mutable && canManage && unstarted(c.status) && <>
        {(c.status === "CREATED" || c.status === "PREPARING") && <ActionDialog label="Prepare" description="Prepare the provider room. The conference moves to WAITING after confirmation." submit={() => prepare.mutateAsync(c.id)} />}
        <ActionDialog label="Start" description="Start this conference and enable live admission." submit={() => start.mutateAsync(c.id)} />
        {c.status === "CREATED" && <ActionDialog label="Reschedule" description="Change only this occurrence. Enter new dates in your browser timezone." submit={() => reschedule.mutateAsync(scheduledWindow(newStart, newEnd))}><Field label="New start" type="datetime-local" required value={newStart} onChange={e => setNewStart(e.target.value)} /><Field label="New end" type="datetime-local" required value={newEnd} onChange={e => setNewEnd(e.target.value)} /></ActionDialog>}
        <ActionDialog label="Cancel occurrence" description="Cancel this unstarted occurrence. For a recurring meeting, the rest of the series is preserved." destructive submit={() => cancel.mutateAsync(c.id)} />
      </>}
      {mutable && c.status === "LIVE" && <>{conferencePermission(space, "lock") && <ActionDialog label={c.isLocked ? "Unlock" : "Lock"} description={c.isLocked ? "Allow new entrants according to the space admission policy." : "Block new non-exempt entrants. Existing participants stay connected."} submit={() => (c.isLocked ? unlock : lock).mutateAsync(c.id)} />}{conferencePermission(space, "end") && <ActionDialog label="End for everyone" description="Stop admission and request provider termination. The conference remains ENDING until termination is confirmed." destructive submit={() => end.mutateAsync(c.id)} />}</>}
    </div>
  </article>;
}
