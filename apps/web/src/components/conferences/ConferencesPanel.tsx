"use client";

import { useRef, useState } from "react";
import Link from "next/link";
import type { ConferenceResponse, MeetingSpaceResponse } from "@/types/v2";
import { useConferences, useCreateConference, usePrepareConference, useStartConference, useLockConference, useUnlockConference, useEndConference, useCancelConference, useRescheduleConference } from "@/hooks/v2/useConferences";
import { useCalendarDownload } from "@/hooks/v2/useCalendar";
import { unstarted, joinable, conferencePermission } from "@/lib/meeting-scheduling";
import { occurrenceWindowInZone, utcToZonedLocal, validTimeZone, zonedDateRange } from "@/lib/meeting-time";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";
import { ScheduleText, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";
import { CalendarPlus, Lock, Repeat, Video } from "lucide-react";
import { cn } from "@/lib/utils";

const STATUS: Record<string, { label: string; tone: string }> = {
  CREATED: { label: "Scheduled", tone: "bg-primary/10 text-primary" },
  PREPARING: { label: "Preparing", tone: "bg-amber-500/10 text-amber-700 dark:text-amber-300" },
  WAITING: { label: "Ready", tone: "bg-amber-500/10 text-amber-700 dark:text-amber-300" },
  LIVE: { label: "Live", tone: "bg-[var(--success)]/12 text-[var(--success)]" },
  ENDING: { label: "Ending", tone: "bg-muted text-muted-foreground" },
  ENDED: { label: "Ended", tone: "bg-muted text-muted-foreground" },
  CANCELLED: { label: "Cancelled", tone: "bg-destructive/10 text-destructive" },
};

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
    <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-base font-semibold text-foreground">Sessions</h2><p className="text-sm text-muted-foreground">Each time this meeting happens. Times are in {zone.replaceAll("_", " ")}, with your own time where it differs.</p></div>
      {canManage && <ActionDialog label="Add a session" onOpen={() => { idempotencyKey.current = crypto.randomUUID(); }} description={`Add one session to this meeting. Leave the start empty for a session without a set time. Times are in ${zone}.`} submit={async () => {
        if (!start && end) throw new Error("Choose a start before an end.");
        await create.mutateAsync({ data: start ? occurrenceWindowInZone(start, end || null, zone) : {}, idempotencyKey: idempotencyKey.current });
        setStart(""); setEnd("");
      }}>
        <ZonedDateTimeField id={`occ-start-${space.id}`} label="Start (optional)" value={start} onChange={setStart} timeZone={zone} />
        <ZonedDateTimeField id={`occ-end-${space.id}`} label="End (optional)" value={end} min={start || undefined} disabled={!start} onChange={setEnd} timeZone={zone} />
      </ActionDialog>}
    </div>
    <div className="grid gap-3 rounded-2xl border border-border bg-card p-4 sm:grid-cols-2"><Field label="From" type="date" value={from} onChange={e => setFrom(e.target.value)} /><Field label="Until" type="date" min={from} value={to} onChange={e => setTo(e.target.value)} /></div>
    {query.isLoading && <div aria-busy="true" className="space-y-3">{[0, 1].map(i => <div key={i} className="h-24 animate-pulse rounded-2xl bg-muted" />)}</div>}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {!query.isError && query.data?.items.length === 0 && <div className="flex flex-col items-center rounded-2xl border border-border bg-card px-6 py-12 text-center"><span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><CalendarPlus className="size-5" /></span><p className="mt-4 font-semibold text-foreground">No sessions{from || to ? " in these dates" : " yet"}</p><p className="mt-1 text-sm text-muted-foreground">{from || to ? "Try different dates." : "Add a session to give this meeting a date."}</p></div>}
    {query.data?.items.map(conference => <ConferenceRow key={conference.id} conference={conference} space={space} canManage={canManage} />)}
  </section>;
}

export function ConferenceRow({ conference: c, space, canManage, onDetailsPage = false }: { conference: ConferenceResponse; space: MeetingSpaceResponse; canManage: boolean; onDetailsPage?: boolean }) {
  const zone = spaceZone(space);
  const prepare = usePrepareConference(), start = useStartConference(), lock = useLockConference(), unlock = useUnlockConference(), end = useEndConference(), cancel = useCancelConference(), reschedule = useRescheduleConference(c.id);
  const download = useCalendarDownload();
  const [newStart, setNewStart] = useState("");
  const [newEnd, setNewEnd] = useState("");
  const mutable = space.status === "ACTIVE" && c.view === "FULL" && !c.isLegacy;
  const recurring = !!c.occurrenceKey?.startsWith("rrule:");
  const st = STATUS[c.status] ?? { label: c.status, tone: "bg-muted text-muted-foreground" };
  return <article className="space-y-4 rounded-2xl border border-border bg-card p-5">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div className="flex min-w-0 items-start gap-3">
        <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", c.status === "LIVE" ? "bg-[var(--success)]/12 text-[var(--success)]" : "bg-primary/10 text-primary")}><Video className="size-5" /></span>
        <div className="min-w-0">
          {c.scheduledStartAt ? <ScheduleText start={c.scheduledStartAt} end={c.scheduledEndAt} timeZone={zone} className="font-semibold text-foreground" /> : <p className="font-semibold text-foreground">No set time</p>}
          <div className="mt-1.5 flex flex-wrap gap-1.5 text-xs">
            {recurring && <span className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 text-muted-foreground"><Repeat className="size-3" /> Repeating</span>}
            {c.isException && <span className="rounded-md bg-muted px-2 py-0.5 text-muted-foreground">Changed</span>}
          </div>
        </div>
      </div>
      <span className={cn("inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold", st.tone)}>
        {c.status === "LIVE" && <span className="size-1.5 animate-pulse rounded-full bg-[var(--success)]" />}
        {st.label}{c.isLocked && <><Lock className="ml-0.5 size-3" /> Locked</>}
      </span>
    </div>
    {c.status === "ENDING" && <p className="rounded-xl bg-muted/60 px-3 py-2 text-sm text-muted-foreground">Ending. No one new can join while the call closes.</p>}
    <div className="flex flex-wrap gap-2 border-t border-border pt-4">
      {space.status === "ACTIVE" && joinable(c.status) && <Link className="inline-flex h-9 items-center gap-1.5 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90" href={`/conferences/${encodeURIComponent(c.id)}`}><Video className="size-4" /> Join</Link>}
      {!onDetailsPage && <Link className="inline-flex h-9 items-center rounded-xl border border-border bg-card px-3.5 text-sm font-semibold text-foreground hover:bg-muted" href={`/dashboard/meetings/${space.id}/conferences/${c.id}`}>Details &amp; people</Link>}
      {c.scheduledStartAt && <button type="button" className="inline-flex h-9 items-center gap-1.5 rounded-xl px-3 text-sm font-medium text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50" disabled={download.isPending} onClick={() => download.mutate({ kind: "conference", id: c.id, name: space.title || space.meetingCode })}><CalendarPlus className="size-4" /> Add to calendar</button>}
      {mutable && canManage && unstarted(c.status) && <>
        {(c.status === "CREATED" || c.status === "PREPARING") && <ActionDialog label="Prepare" description="Prepare the provider room. The conference moves to WAITING after confirmation." submit={() => prepare.mutateAsync(c.id)} />}
        <ActionDialog label="Start" description="Start this conference and enable live admission. Scheduled occurrences can start from the early-start window." submit={() => start.mutateAsync(c.id)} />
        {c.status === "CREATED" && <ActionDialog label="Reschedule" onOpen={() => { setNewStart(c.scheduledStartAt ? utcToZonedLocal(c.scheduledStartAt, zone) : ""); setNewEnd(c.scheduledEndAt ? utcToZonedLocal(c.scheduledEndAt, zone) : ""); }} description={`Move only this session${recurring ? "; the rest of the series stays the same" : ""}. Times are in ${zone}. Leave the end empty to keep the same length.`} submit={() => reschedule.mutateAsync({ ...occurrenceWindowInZone(newStart, newEnd || null, zone), ...(c.version != null ? { version: c.version } : {}) })}>
          <ZonedDateTimeField id={`resched-start-${c.id}`} label="New start" required value={newStart} onChange={setNewStart} timeZone={zone} />
          <ZonedDateTimeField id={`resched-end-${c.id}`} label="New end (optional)" value={newEnd} min={newStart} onChange={setNewEnd} timeZone={zone} />
        </ActionDialog>}
        <ActionDialog label="Cancel session" description={recurring ? "Cancel just this session. The rest of the repeating series stays." : "Cancel this session before it starts."} destructive submit={() => cancel.mutateAsync(c.id)} />
      </>}
      {c.view === "FULL" && !c.isLegacy && c.status === "LIVE" && <>{space.status === "ACTIVE" && conferencePermission(space, "lock") && <ActionDialog label={c.isLocked ? "Unlock" : "Lock"} description={c.isLocked ? "Allow new entrants according to the space admission policy." : "Block new non-exempt entrants. Existing participants stay connected."} submit={() => (c.isLocked ? unlock : lock).mutateAsync(c.id)} />}{conferencePermission(space, "end") && <ActionDialog label="End for everyone" description="Stop admission and request provider termination. The conference remains ENDING until termination is confirmed." destructive submit={() => end.mutateAsync(c.id)} />}</>}
    </div>
    {download.isError && <RequestError error={download.error} />}
  </article>;
}
