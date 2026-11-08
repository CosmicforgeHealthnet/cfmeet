"use client";

import { useRef, useState } from "react";
import Link from "next/link";
import type { ConferenceResponse, MeetingSpaceResponse } from "@/types/v2";
import { useConferences, useCreateConference, usePrepareConference, useStartConference, useLockConference, useUnlockConference, useEndConference, useCancelConference, useRescheduleConference } from "@/hooks/v2/useConferences";
import { useCalendarDownload } from "@/hooks/v2/useCalendar";
import { unstarted, joinable, conferencePermission } from "@/lib/meeting-scheduling";
import { occurrenceWindowInZone, utcToZonedLocal, validTimeZone, zonedDateRange } from "@/lib/meeting-time";
import { Button } from "@/components/ui/button";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";
import { ScheduleText, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";

/** The meeting timezone for typing and showing occurrence times (MTG-012). */
export function spaceZone(space: Pick<MeetingSpaceResponse, "timezone">): string {
  return validTimeZone(space.timezone) ? (space.timezone as string) : "UTC";
}

export default function ConferencesPanel({ space }: { space: MeetingSpaceResponse }) {
  const zone = spaceZone(space);
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  // One idempotency key per "Schedule occurrence" dialog: a retried submit returns the same occurrence.
  const idempotencyKey = useRef("");
  // Date filters are whole days in the meeting zone; `to` is the exclusive next midnight (MTG-021).
  const query = useConferences(space.id, zonedDateRange(from, to, zone));
  const create = useCreateConference(space.id);
  const canManage = conferencePermission(space, "start");
  return <section id="conferences" className="space-y-4">
    <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-lg font-semibold">{space.title || space.meetingCode} — occurrences</h2><p className="text-sm text-muted-foreground">Each occurrence has its own lifecycle. Times are in the meeting timezone ({zone}), with your local time where it differs.</p></div>
      {canManage && <ActionDialog label="Schedule occurrence" onOpen={() => { idempotencyKey.current = crypto.randomUUID(); }} description={`Create one occurrence. Leave the start empty for an on-demand occurrence without a date. Times are in ${zone}.`} submit={async () => {
        if (!start && end) throw new Error("Choose a start before an end.");
        await create.mutateAsync({ data: start ? occurrenceWindowInZone(start, end || null, zone) : {}, idempotencyKey: idempotencyKey.current });
        setStart(""); setEnd("");
      }}>
        <ZonedDateTimeField id={`occ-start-${space.id}`} label="Start (optional)" value={start} onChange={setStart} timeZone={zone} />
        <ZonedDateTimeField id={`occ-end-${space.id}`} label="End (optional)" value={end} min={start || undefined} disabled={!start} onChange={setEnd} timeZone={zone} />
      </ActionDialog>}
    </div>
    <div className="grid gap-3 sm:grid-cols-2"><Field label={`From date (${zone})`} type="date" value={from} onChange={e => setFrom(e.target.value)} /><Field label={`Through date (${zone})`} type="date" min={from} value={to} onChange={e => setTo(e.target.value)} /></div>
    {query.isLoading && <p role="status">Loading occurrences…</p>}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {!query.isError && query.data?.items.length === 0 && <p className="rounded-xl border border-dashed p-8 text-center text-muted-foreground">No occurrences in this date range.</p>}
    {query.data?.items.map(conference => <ConferenceRow key={conference.id} conference={conference} space={space} canManage={canManage} />)}
  </section>;
}

export function ConferenceRow({ conference: c, space, canManage }: { conference: ConferenceResponse; space: MeetingSpaceResponse; canManage: boolean }) {
  const zone = spaceZone(space);
  const prepare = usePrepareConference(), start = useStartConference(), lock = useLockConference(), unlock = useUnlockConference(), end = useEndConference(), cancel = useCancelConference(), reschedule = useRescheduleConference(c.id);
  const download = useCalendarDownload();
  const [newStart, setNewStart] = useState("");
  const [newEnd, setNewEnd] = useState("");
  const mutable = space.status === "ACTIVE" && c.view === "FULL" && !c.isLegacy;
  const recurring = !!c.occurrenceKey?.startsWith("rrule:");
  return <article className="space-y-3 rounded-xl border border-border bg-card p-4">
    <div className="flex flex-wrap items-center justify-between gap-2"><div>{c.scheduledStartAt ? <ScheduleText start={c.scheduledStartAt} end={c.scheduledEndAt} timeZone={zone} className="font-medium" /> : <p className="font-medium">On-demand occurrence (no date)</p>}<p className="text-xs text-muted-foreground break-all">{c.id} · {c.kind || "Conference"}{recurring ? " · Recurring series" : ""}{c.isException ? " · Changed occurrence" : ""}</p></div><span className="rounded-full bg-muted px-3 py-1 text-xs font-medium">{c.status}{c.isLocked ? " · Locked" : ""}</span></div>
    {c.status === "ENDING" && <p className="text-sm text-muted-foreground">Admission has stopped. Waiting for the media provider to confirm termination.</p>}
    <div className="flex flex-wrap gap-2">
      <Button size="sm" variant="outline" asChild><Link href={`/dashboard/meetings/${space.id}/conferences/${c.id}`}>Details &amp; participants</Link></Button>
      {space.status === "ACTIVE" && joinable(c.status) && <Button size="sm" asChild><Link href={`/conferences/${encodeURIComponent(c.id)}`}>Join occurrence</Link></Button>}
      {c.scheduledStartAt && <Button size="sm" variant="ghost" disabled={download.isPending} onClick={() => download.mutate({ kind: "conference", id: c.id, name: space.title || space.meetingCode })}>Add to calendar</Button>}
      {mutable && canManage && unstarted(c.status) && <>
        {(c.status === "CREATED" || c.status === "PREPARING") && <ActionDialog label="Prepare" description="Prepare the provider room. The conference moves to WAITING after confirmation." submit={() => prepare.mutateAsync(c.id)} />}
        <ActionDialog label="Start" description="Start this conference and enable live admission. Scheduled occurrences can start from the early-start window." submit={() => start.mutateAsync(c.id)} />
        {c.status === "CREATED" && <ActionDialog label="Reschedule" onOpen={() => { setNewStart(c.scheduledStartAt ? utcToZonedLocal(c.scheduledStartAt, zone) : ""); setNewEnd(c.scheduledEndAt ? utcToZonedLocal(c.scheduledEndAt, zone) : ""); }} description={`Move only this occurrence${recurring ? "; the rest of the series is unchanged" : ""}. Times are in ${zone}. Leave the end empty to keep the current duration.`} submit={() => reschedule.mutateAsync({ ...occurrenceWindowInZone(newStart, newEnd || null, zone), ...(c.version != null ? { version: c.version } : {}) })}>
          <ZonedDateTimeField id={`resched-start-${c.id}`} label="New start" required value={newStart} onChange={setNewStart} timeZone={zone} />
          <ZonedDateTimeField id={`resched-end-${c.id}`} label="New end (optional)" value={newEnd} min={newStart} onChange={setNewEnd} timeZone={zone} />
        </ActionDialog>}
        <ActionDialog label="Cancel occurrence" description={recurring ? "Cancel this occurrence only. The rest of the series is preserved." : "Cancel this unstarted occurrence."} destructive submit={() => cancel.mutateAsync(c.id)} />
      </>}
      {mutable && c.status === "LIVE" && <>{conferencePermission(space, "lock") && <ActionDialog label={c.isLocked ? "Unlock" : "Lock"} description={c.isLocked ? "Allow new entrants according to the space admission policy." : "Block new non-exempt entrants. Existing participants stay connected."} submit={() => (c.isLocked ? unlock : lock).mutateAsync(c.id)} />}{conferencePermission(space, "end") && <ActionDialog label="End for everyone" description="Stop admission and request provider termination. The conference remains ENDING until termination is confirmed." destructive submit={() => end.mutateAsync(c.id)} />}</>}
    </div>
    {download.isError && <RequestError error={download.error} />}
  </article>;
}
