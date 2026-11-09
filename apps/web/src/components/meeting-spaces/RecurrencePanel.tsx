"use client";

import { useState } from "react";
import type { MeetingSpaceResponse, RecurrenceResponse } from "@/types/v2";
import { useSpaceRecurrence, useSpaceExceptions, useSetRecurrence, useDeleteRecurrence, useSetException } from "@/hooks/v2/useSpaces";
import { recurrenceInput, conferencePermission } from "@/lib/meeting-scheduling";
import { formatInZone, localTimeKind, occurrenceWindowInZone, zonedLocalToUtc } from "@/lib/meeting-time";
import { spaceZone } from "@/components/conferences/ConferencesPanel";
import { ScheduleText, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

export default function RecurrencePanel({ space }: { space: MeetingSpaceResponse }) {
  const zone = spaceZone(space);
  const rule = useSpaceRecurrence(space.id);
  const exceptions = useSpaceExceptions(rule.data ? space.id : "");
  const remove = useDeleteRecurrence(space.id);
  const canManage = conferencePermission(space, "start");
  return <section className="space-y-4">
    <div><h2 className="text-lg font-semibold">Recurring schedule</h2><p className="text-sm text-muted-foreground">Recurrence follows {zone} wall-clock time, including daylight saving changes. Occurrences beyond the generation horizon still appear in the calendar.</p></div>
    {rule.isLoading && <p role="status">Loading recurring schedule…</p>}
    {rule.isError && <RequestError error={rule.error} retry={() => void rule.refetch()} />}
    {rule.isSuccess && <>
      <div className="rounded-xl border border-border bg-card p-4 space-y-3">
        {rule.data ? <><p className="font-mono text-sm break-all">{rule.data.rrule}</p><p className="text-sm">First start: {rule.data.startLocal.slice(0, 16).replace("T", " ")} ({rule.data.timezone})</p><p className="text-sm text-muted-foreground">{rule.data.durationMinutes} minutes per occurrence{rule.data.materializedUntil ? ` · Generated through ${formatInZone(rule.data.materializedUntil, rule.data.timezone)}` : ""}</p></> : <p className="text-muted-foreground">No active recurring series.</p>}
        {canManage && <div className="flex flex-wrap gap-2"><RecurrenceEditor key={rule.data?.id || "new"} space={space} rule={rule.data} />{rule.data && <ActionDialog label="End series" destructive description="End the recurring series. Every unstarted future occurrence is retired, rescheduled ones included. Past occurrences are preserved." submit={() => remove.mutateAsync()} />}</div>}
      </div>
      {rule.data && <><div className="flex flex-wrap items-center justify-between gap-3"><h3 className="font-semibold">Single-occurrence changes</h3>{canManage && <ExceptionEditor spaceId={space.id} zone={rule.data.timezone || zone} />}</div>
        {exceptions.isLoading && <p role="status">Loading exceptions…</p>}
        {exceptions.isError && <RequestError error={exceptions.error} retry={() => void exceptions.refetch()} />}
        {exceptions.data?.length === 0 && <p className="text-sm text-muted-foreground">No changed occurrences in the active series.</p>}
        {exceptions.data?.map(item => <article key={item.id} className="rounded-xl border border-border p-4 text-sm space-y-1"><p className="font-medium">{item.kind === "CANCELLED" ? "Cancelled" : "Rescheduled"} · {item.originalStartLocal.slice(0, 16).replace("T", " ")} ({item.timezone})</p>{item.newStartAt && <div>New time: <ScheduleText start={item.newStartAt} end={item.newEndAt} timeZone={item.timezone} /></div>}</article>)}
      </>}
    </>}
  </section>;
}

function RecurrenceEditor({ space, rule }: { space: MeetingSpaceResponse; rule: RecurrenceResponse | null | undefined }) {
  const zone = spaceZone(space);
  const [rrule, setRrule] = useState(rule?.rrule || "FREQ=WEEKLY;BYDAY=MO;COUNT=10");
  const [local, setLocal] = useState(rule?.startLocal.slice(0, 16) || "");
  const [duration, setDuration] = useState(rule?.durationMinutes || 60);
  const save = useSetRecurrence(space.id);
  return <ActionDialog label={rule ? "Edit series" : "Create recurring series"} description={rule ? "Edit the active series in place. Changed occurrences keep their changes; occurrences the new rule no longer produces are removed. Past occurrences are never touched." : "Enter an RFC 5545 RRULE body. The first start is wall-clock time in the meeting timezone."} submit={async () => {
    if (local && localTimeKind(local, zone) !== "ok") throw new Error("A series cannot start at a time that is skipped or repeated by a daylight saving change.");
    await save.mutateAsync(recurrenceInput(rrule, local, duration));
  }}>
    <ZonedDateTimeField id={`series-start-${space.id}`} label="First start" required recurringStart value={local} onChange={setLocal} timeZone={zone} />
    <Field label="RRULE" required value={rrule} onChange={e => setRrule(e.target.value)} placeholder="FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10" />
    <p className="text-xs text-muted-foreground">Use COUNT or UNTIL to bound the series. Examples: FREQ=DAILY;COUNT=5 or FREQ=MONTHLY;BYMONTHDAY=1;COUNT=12.</p>
    <Field label="Duration (minutes)" type="number" min="1" max="1440" required value={duration} onChange={e => setDuration(Number(e.target.value))} />
  </ActionDialog>;
}

function ExceptionEditor({ spaceId, zone }: { spaceId: string; zone: string }) {
  const save = useSetException(spaceId);
  const [original, setOriginal] = useState("");
  const [action, setAction] = useState<"CANCEL" | "RESCHEDULE">("CANCEL");
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  return <ActionDialog label="Change one occurrence" description={`Cancel or move one future occurrence of the series, including one not generated yet. Pick its original date and time as the series produces it (${zone}). Other occurrences stay unchanged. The calendar offers the same actions per occurrence.`} submit={() => save.mutateAsync({
    originalStartAt: zonedLocalToUtc(original, zone),
    action,
    ...(action === "RESCHEDULE" ? occurrenceWindowInZone(start, end || null, zone) : {}),
  })}>
    <ZonedDateTimeField id={`exception-original-${spaceId}`} label="Original start" required value={original} onChange={setOriginal} timeZone={zone} />
    <label className="grid gap-2 text-sm font-medium">Action<select className="h-10 rounded-md border border-input bg-background px-3" value={action} onChange={e => setAction(e.target.value as typeof action)}><option value="CANCEL">Cancel occurrence</option><option value="RESCHEDULE">Reschedule occurrence</option></select></label>
    {action === "RESCHEDULE" && <>
      <ZonedDateTimeField id={`exception-start-${spaceId}`} label="New start" required value={start} onChange={setStart} timeZone={zone} />
      <ZonedDateTimeField id={`exception-end-${spaceId}`} label="New end (optional, defaults to the series duration)" value={end} min={start} onChange={setEnd} timeZone={zone} />
    </>}
  </ActionDialog>;
}
