"use client";

import { useState } from "react";
import type { MeetingSpaceResponse, RecurrenceResponse } from "@/types/v2";
import { useSpaceRecurrence, useSpaceExceptions, useSetRecurrence, useDeleteRecurrence, useSetException } from "@/hooks/v2/useSpaces";
import { recurrenceInput, occurrenceTimestamp, scheduledWindow, conferencePermission } from "@/lib/meeting-scheduling";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

export default function RecurrencePanel({ space }: { space: MeetingSpaceResponse }) {
  const rule = useSpaceRecurrence(space.id);
  const exceptions = useSpaceExceptions(rule.data ? space.id : "");
  const remove = useDeleteRecurrence(space.id);
  const canManage = conferencePermission(space, "start");
  return <section className="space-y-4">
    <div><h2 className="text-lg font-semibold">Recurring schedule</h2><p className="text-sm text-muted-foreground">Recurrence follows {space.timezone || "the meeting space timezone"}, including daylight saving changes. The server generates upcoming occurrences within its scheduling horizon.</p></div>
    {rule.isLoading && <p role="status">Loading recurring schedule…</p>}
    {rule.isError && <RequestError error={rule.error} retry={() => void rule.refetch()} />}
    {rule.isSuccess && <>
      <div className="rounded-xl border border-border bg-card p-4 space-y-3">
        {rule.data ? <><p className="font-mono text-sm break-all">{rule.data.rrule}</p><p className="text-sm">First local start: {rule.data.startLocal.replace("T", " ")} ({rule.data.timezone})</p><p className="text-sm text-muted-foreground">{rule.data.durationMinutes} minutes per occurrence{rule.data.materializedUntil ? ` · Generated through ${new Date(rule.data.materializedUntil).toLocaleString()}` : ""}</p></> : <p className="text-muted-foreground">No active recurring series.</p>}
        {canManage && <div className="flex flex-wrap gap-2"><RecurrenceEditor key={rule.data?.id || "new"} space={space} rule={rule.data} />{rule.data && <ActionDialog label="End series" destructive description="End the recurring series and retire its future ordinary occurrences. Past and exception occurrences are preserved." submit={() => remove.mutateAsync()} />}</div>}
      </div>
      {rule.data && <><div className="flex flex-wrap items-center justify-between gap-3"><h3 className="font-semibold">Single-occurrence exceptions</h3>{canManage && <ExceptionEditor spaceId={space.id} />}</div>
        {exceptions.isLoading && <p role="status">Loading exceptions…</p>}
        {exceptions.isError && <RequestError error={exceptions.error} retry={() => void exceptions.refetch()} />}
        {exceptions.data?.length === 0 && <p className="text-sm text-muted-foreground">No exceptions in the active series.</p>}
        {exceptions.data?.map(item => <article key={item.id} className="rounded-xl border border-border p-4 text-sm space-y-1"><p className="font-medium">{item.kind === "CANCELLED" ? "Cancelled" : "Rescheduled"} · {item.originalStartLocal.replace("T", " ")} ({item.timezone})</p>{item.newStartAt && <p>New start: {new Date(item.newStartAt).toLocaleString()}</p>}{item.newEndAt && <p>New end: {new Date(item.newEndAt).toLocaleString()}</p>}</article>)}
      </>}
    </>}
  </section>;
}

function RecurrenceEditor({ space, rule }: { space: MeetingSpaceResponse; rule: RecurrenceResponse | null | undefined }) {
  const [rrule, setRrule] = useState(rule?.rrule || "FREQ=WEEKLY;BYDAY=MO;COUNT=10");
  const [local, setLocal] = useState(rule?.startLocal.slice(0, 19) || "");
  const [duration, setDuration] = useState(rule?.durationMinutes || 60);
  const save = useSetRecurrence(space.id);
  return <ActionDialog label={rule ? "Replace series" : "Create recurring series"} description={rule ? "Replace the active rule and retire its future ordinary occurrences. Review the first local start, timezone and rule before saving." : "Enter an RFC 5545 RRULE body. The first start is interpreted directly in the meeting space timezone."} submit={() => save.mutateAsync(recurrenceInput(rrule, local, duration))}>
    <Field label={`First start (${space.timezone})`} type="datetime-local" step="1" required value={local} onChange={e => setLocal(e.target.value)} />
    <Field label="RRULE" required value={rrule} onChange={e => setRrule(e.target.value)} placeholder="FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10" />
    <p className="text-xs text-muted-foreground">Use COUNT or UNTIL to bound the series. Examples: FREQ=DAILY;COUNT=5 or FREQ=MONTHLY;BYMONTHDAY=1;COUNT=12.</p>
    <Field label="Duration (minutes)" type="number" min="1" max="1440" required value={duration} onChange={e => setDuration(Number(e.target.value))} />
  </ActionDialog>;
}

function ExceptionEditor({ spaceId }: { spaceId: string }) {
  const save = useSetException(spaceId);
  const [original, setOriginal] = useState("");
  const [action, setAction] = useState<"CANCEL" | "RESCHEDULE">("CANCEL");
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  return <ActionDialog label="Add exception" description="Cancel or move one future occurrence, including one not generated yet. Use its exact original timestamp with a timezone offset. Other occurrences stay unchanged." submit={() => save.mutateAsync({ originalStartAt: occurrenceTimestamp(original), action, ...(action === "RESCHEDULE" ? scheduledWindow(start, end) : {}) })}>
    <Field label="Original start (ISO timestamp with offset)" required placeholder="2027-01-04T09:00:00+01:00" value={original} onChange={e => setOriginal(e.target.value)} />
    <label className="grid gap-2 text-sm font-medium">Action<select className="h-10 rounded-md border border-input bg-background px-3" value={action} onChange={e => setAction(e.target.value as typeof action)}><option value="CANCEL">Cancel occurrence</option><option value="RESCHEDULE">Reschedule occurrence</option></select></label>
    {action === "RESCHEDULE" && <><p className="text-xs text-muted-foreground">New dates use your browser timezone: {Intl.DateTimeFormat().resolvedOptions().timeZone}.</p><Field label="New start" type="datetime-local" required value={start} onChange={e => setStart(e.target.value)} /><Field label="New end" type="datetime-local" required value={end} onChange={e => setEnd(e.target.value)} /></>}
  </ActionDialog>;
}
