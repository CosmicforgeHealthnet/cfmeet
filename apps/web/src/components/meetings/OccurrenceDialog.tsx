"use client";

/**
 * One agenda occurrence: schedule in the meeting zone and the viewer's zone, and the controls
 * the agenda says the caller may use (`occurrenceActions`). Every action is re-checked by the
 * backend; errors are shown as returned.
 *
 *  - Join → `/conferences/{conferenceId}` (pre-join keyed by conference id, FE-017)
 *  - Reschedule this occurrence → POST /api/v2/conferences/{id}/reschedule
 *  - Cancel this occurrence → POST /api/v2/conferences/{id}/cancel
 *  - Not yet generated recurring occurrence → PUT /api/v2/meeting-spaces/{id}/recurrence/exceptions
 *  - Edit the whole series → the space page (PUT /api/v2/meeting-spaces/{id}/recurrence)
 *  - Add to calendar → GET /api/v2/conferences/{id}/calendar.ics, /meeting-spaces/{id}/calendar.ics
 */
import { useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCalendarDownload, useCancelOccurrence, useOccurrenceException, useRescheduleOccurrence } from "@/hooks/v2/useCalendar";
import { occurrenceActions } from "@/lib/meeting-calendar";
import { occurrenceWindowInZone, utcToZonedLocal, validTimeZone, viewerTimeZone } from "@/lib/meeting-time";
import type { AgendaItem } from "@/types/generated/meeting-service";
import { ScheduleText, ZonedDateTimeField } from "./ScheduleFields";

type Mode = "view" | "reschedule" | "cancel" | "exception-cancel" | "exception-reschedule";

export default function OccurrenceDialog({ item, onClose }: { item: AgendaItem; onClose: () => void }) {
  const actions = occurrenceActions(item);
  const zone = validTimeZone(item.meetingSpace.timezone) ? item.meetingSpace.timezone : viewerTimeZone();
  const [mode, setMode] = useState<Mode>("view");
  const [start, setStart] = useState(() => utcToZonedLocal(item.scheduledStartAt, zone));
  const [end, setEnd] = useState(() => (item.scheduledEndAt ? utcToZonedLocal(item.scheduledEndAt, zone) : ""));
  const [error, setError] = useState<unknown>();
  const reschedule = useRescheduleOccurrence();
  const cancel = useCancelOccurrence();
  const exception = useOccurrenceException();
  const download = useCalendarDownload();
  const pending = reschedule.isPending || cancel.isPending || exception.isPending;
  const name = item.meetingSpace.title || "CF Meet meeting";
  const manager = actions.reschedule || actions.cancel || actions.exception;

  const submit = async () => {
    setError(undefined);
    try {
      if (mode === "reschedule" && item.conferenceId) {
        // An empty end keeps the occurrence's duration (backend rule).
        await reschedule.mutateAsync({ id: item.conferenceId, data: occurrenceWindowInZone(start, end || null, zone) });
      } else if (mode === "cancel" && item.conferenceId) {
        await cancel.mutateAsync(item.conferenceId);
      } else if ((mode === "exception-cancel" || mode === "exception-reschedule") && actions.originalStartAt) {
        await exception.mutateAsync({
          spaceId: item.meetingSpaceId,
          data: mode === "exception-cancel"
            ? { action: "CANCEL", originalStartAt: actions.originalStartAt }
            : { action: "RESCHEDULE", originalStartAt: actions.originalStartAt, ...occurrenceWindowInZone(start, end || null, zone) },
        });
      }
      onClose();
    } catch (cause) {
      setError(cause);
    }
  };

  return <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose(); }}>
    <DialogContent className="max-w-lg">
      <DialogHeader>
        <DialogTitle>{name}</DialogTitle>
        <DialogDescription asChild><div className="space-y-1 text-sm">
          <ScheduleText start={item.scheduledStartAt} end={item.scheduledEndAt} timeZone={zone} className="text-foreground" />
          <p>{item.status}{item.isException ? " · changed occurrence" : ""}{actions.recurring ? " · part of a recurring series" : ""}</p>
          {!item.materialized ? <p>This recurring occurrence has not been generated yet. Join and start become available once it is.</p> : null}
          <p className="font-mono text-xs">{item.meetingSpace.meetingCode}</p>
        </div></DialogDescription>
      </DialogHeader>

      {mode === "view" ? <div className="flex flex-wrap gap-2">
        {actions.join && item.conferenceId ? <Button asChild><Link href={`/conferences/${encodeURIComponent(item.conferenceId)}`}>{item.status === "LIVE" ? "Join now" : "Open pre-join"}</Link></Button> : null}
        {actions.ics && item.conferenceId ? <Button variant="outline" disabled={download.isPending} onClick={() => download.mutate({ kind: "conference", id: item.conferenceId as string, name })}>Add to calendar (.ics)</Button> : null}
        {actions.recurring ? <Button variant="outline" disabled={download.isPending} onClick={() => download.mutate({ kind: "space", id: item.meetingSpaceId, name })}>Download series (.ics)</Button> : null}
        {actions.reschedule ? <Button variant="outline" onClick={() => setMode("reschedule")}>{actions.recurring ? "Reschedule this occurrence" : "Reschedule"}</Button> : null}
        {actions.cancel ? <Button variant="destructive" onClick={() => setMode("cancel")}>{actions.recurring ? "Cancel this occurrence" : "Cancel meeting"}</Button> : null}
        {actions.exception ? <>
          <Button variant="outline" onClick={() => setMode("exception-reschedule")}>Reschedule this occurrence</Button>
          <Button variant="destructive" onClick={() => setMode("exception-cancel")}>Cancel this occurrence</Button>
        </> : null}
        {manager && actions.recurring ? <Button variant="ghost" asChild><Link href={`/dashboard/meetings/${encodeURIComponent(item.meetingSpaceId)}#recurrence`}>Edit series</Link></Button> : null}
        {item.view === "FULL" ? <Button variant="ghost" asChild><Link href={`/dashboard/meetings/${encodeURIComponent(item.meetingSpaceId)}`}>Open meeting space</Link></Button> : null}
      </div> : <form className="space-y-4" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        {mode === "reschedule" || mode === "exception-reschedule" ? <>
          <p className="text-sm text-muted-foreground">Only this occurrence moves{actions.recurring ? "; the rest of the series is unchanged" : ""}. Times are in the meeting timezone. Leave the end empty to keep the current duration.</p>
          <ZonedDateTimeField id="occurrence-start" label="New start" required value={start} onChange={setStart} timeZone={zone} />
          <ZonedDateTimeField id="occurrence-end" label="New end (optional)" value={end} min={start} onChange={setEnd} timeZone={zone} />
          {start ? <p className="text-xs text-muted-foreground">Your time: {(() => { try { return <ScheduleText start={occurrenceWindowInZone(start, null, zone, 0).scheduledStartAt} timeZone={viewerTimeZone()} />; } catch { return "—"; } })()}</p> : null}
        </> : <p className="text-sm">{mode === "cancel" ? "Cancel this occurrence? Participants can no longer join it." : "Cancel this occurrence of the series? Other occurrences stay unchanged."}</p>}
        {error !== undefined ? <RequestError error={error} /> : null}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={pending} onClick={() => { setMode("view"); setError(undefined); }}>Back</Button>
          <Button type="submit" variant={mode.includes("cancel") ? "destructive" : "default"} disabled={pending}>{pending ? "Saving…" : mode.includes("cancel") ? "Cancel occurrence" : "Save new time"}</Button>
        </DialogFooter>
      </form>}
      {download.isError ? <RequestError error={download.error} /> : null}
    </DialogContent>
  </Dialog>;
}
