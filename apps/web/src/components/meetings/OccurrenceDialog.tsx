"use client";

/**
 * One agenda occurrence as an event card (Google Calendar pattern): title, time, status, one
 * primary Join, the meeting link, and icon actions (change time, cancel, more). Every action
 * is re-checked by the backend; errors are shown as returned.
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
import { Popover } from "radix-ui";
import { CalendarPlus, Check, Clock, ExternalLink, Link2, MoreVertical, Pencil, Repeat, Trash2, Video } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCalendarDownload, useCancelOccurrence, useOccurrenceException, useRescheduleOccurrence } from "@/hooks/v2/useCalendar";
import { occurrenceActions } from "@/lib/meeting-calendar";
import { occurrenceWindowInZone, utcToZonedLocal, validTimeZone, viewerTimeZone } from "@/lib/meeting-time";
import { cn } from "@/lib/utils";
import type { AgendaItem } from "@/types/generated/meeting-service";
import { ScheduleText, ZonedDateTimeField } from "./ScheduleFields";

const fmtDay = (iso: string, zone: string) => new Intl.DateTimeFormat(undefined, { weekday: "long", day: "numeric", month: "long", timeZone: zone }).format(new Date(iso));
const fmtClock = (iso: string, zone: string) => new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: zone }).format(new Date(iso));
const fmtRange = (start: string, end: string | null | undefined, zone: string) => (end ? `${fmtClock(start, zone)} – ${fmtClock(end, zone)}` : fmtClock(start, zone));

type Mode = "view" | "reschedule" | "cancel" | "exception-cancel" | "exception-reschedule";

export default function OccurrenceDialog({ item, onClose }: { item: AgendaItem; onClose: () => void }) {
  const actions = occurrenceActions(item);
  const zone = validTimeZone(item.meetingSpace.timezone) ? item.meetingSpace.timezone : viewerTimeZone();
  const [mode, setMode] = useState<Mode>("view");
  const [start, setStart] = useState(() => utcToZonedLocal(item.scheduledStartAt, zone));
  const [end, setEnd] = useState(() => (item.scheduledEndAt ? utcToZonedLocal(item.scheduledEndAt, zone) : ""));
  const [error, setError] = useState<unknown>();
  // Moving the start keeps the meeting's length: the end moves with it.
  const moveStart = (next: string) => {
    const before = Date.parse(`${start}Z`), after = Date.parse(`${next}Z`), ends = Date.parse(`${end}Z`);
    if (end && Number.isFinite(before) && Number.isFinite(after) && Number.isFinite(ends)) {
      setEnd(new Date(ends + (after - before)).toISOString().slice(0, end.length));
    }
    setStart(next);
  };
  const [copied, setCopied] = useState(false);
  const reschedule = useRescheduleOccurrence();
  const cancel = useCancelOccurrence();
  const exception = useOccurrenceException();
  const download = useCalendarDownload();
  const pending = reschedule.isPending || cancel.isPending || exception.isPending;
  const name = item.meetingSpace.title || "CF Meet meeting";
  const manager = actions.reschedule || actions.cancel || actions.exception;

  const viewer = viewerTimeZone();
  const live = item.status === "LIVE";
  const done = item.status === "ENDED" || item.status === "CANCELLED" || item.status === "FAILED";
  const statusText = live ? "Live now" : item.status === "CANCELLED" ? "Cancelled" : item.status === "ENDED" ? "Ended" : item.status === "FAILED" ? "Didn't start" : "Upcoming";
  const one = actions.recurring ? "this session" : "meeting";
  const canMove = actions.reschedule || actions.exception;
  const canCancel = actions.cancel || actions.exception;
  const moveMode: Mode = actions.exception ? "exception-reschedule" : "reschedule";
  const cancelMode: Mode = actions.exception ? "exception-cancel" : "cancel";
  const menuItem = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground hover:bg-muted disabled:opacity-50";
  const iconBtn = "flex size-9 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground";
  const hasMenu = (actions.ics && item.conferenceId) || actions.recurring || item.view === "FULL";

  const copyLink = () => {
    void navigator.clipboard.writeText(`${window.location.origin}/${item.meetingSpace.meetingCode}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

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
    <DialogContent className="gap-0 p-0 sm:max-w-md">
      {mode === "view" ? <>
        <div className="flex min-h-14 items-center justify-end gap-0.5 px-3 pr-12 pt-3">
          {canMove && !done && <button type="button" aria-label={`Change time of ${one}`} title="Change time" onClick={() => setMode(moveMode)} className={iconBtn}><Pencil className="size-4" /></button>}
          {canCancel && !done && <button type="button" aria-label={`Cancel ${one}`} title={`Cancel ${one}`} onClick={() => setMode(cancelMode)} className={cn(iconBtn, "hover:text-destructive")}><Trash2 className="size-4" /></button>}
          {hasMenu && (
            <Popover.Root>
              <Popover.Trigger asChild><button type="button" aria-label="More options" title="More" className={iconBtn}><MoreVertical className="size-4" /></button></Popover.Trigger>
              <Popover.Portal>
                <Popover.Content align="end" sideOffset={6} collisionPadding={12} className="z-[60] w-60 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                  {actions.ics && item.conferenceId && <button type="button" className={menuItem} disabled={download.isPending} onClick={() => download.mutate({ kind: "conference", id: item.conferenceId as string, name })}><CalendarPlus className="size-4 text-muted-foreground" />Add to my calendar</button>}
                  {actions.recurring && <button type="button" className={menuItem} disabled={download.isPending} onClick={() => download.mutate({ kind: "space", id: item.meetingSpaceId, name })}><Repeat className="size-4 text-muted-foreground" />Add the whole series</button>}
                  {manager && actions.recurring && <Link className={menuItem} href={`/dashboard/meetings/${encodeURIComponent(item.meetingSpaceId)}#recurrence`}><Pencil className="size-4 text-muted-foreground" />Edit the series</Link>}
                  {item.view === "FULL" && <Link className={menuItem} href={`/dashboard/meetings/${encodeURIComponent(item.meetingSpaceId)}`}><ExternalLink className="size-4 text-muted-foreground" />Open meeting details</Link>}
                </Popover.Content>
              </Popover.Portal>
            </Popover.Root>
          )}
        </div>
        <div className="px-6 pb-6">
          <DialogHeader className="space-y-0 text-left">
            <div className="flex items-start gap-4">
              <span className={cn("mt-2.5 size-3.5 shrink-0 rounded", live ? "bg-emerald-500" : done ? "bg-muted-foreground/40" : "bg-primary")} />
              <DialogTitle className={cn("text-[22px] font-semibold leading-snug tracking-tight", item.status === "CANCELLED" && "text-muted-foreground line-through")}>{name}</DialogTitle>
            </div>
            <DialogDescription className="sr-only">{statusText}</DialogDescription>
          </DialogHeader>

          <div className="mt-4 space-y-3.5 text-sm">
            <div className="flex gap-4">
              <Clock className="mt-0.5 size-[18px] shrink-0 text-muted-foreground" />
              <div>
                <p className="text-foreground">{fmtDay(item.scheduledStartAt, viewer)}</p>
                <p className="text-muted-foreground">{fmtRange(item.scheduledStartAt, item.scheduledEndAt, viewer)}{zone !== viewer ? ` · ${fmtRange(item.scheduledStartAt, item.scheduledEndAt, zone)} host time` : ""}</p>
              </div>
            </div>
            {actions.recurring && <div className="flex items-center gap-4"><Repeat className="size-[18px] shrink-0 text-muted-foreground" /><p className="text-foreground">Repeats{item.isException ? " · this one was moved" : ""}</p></div>}
            <div className="flex items-center gap-4">
              <span className="flex size-[18px] shrink-0 items-center justify-center"><span className={cn("size-2.5 rounded-full", live ? "animate-pulse bg-emerald-500" : done ? "bg-muted-foreground/40" : "bg-primary")} /></span>
              <p className={cn("text-foreground", live && "font-semibold text-emerald-600 dark:text-emerald-400")}>{statusText}{item.relations.includes("OWNER") ? " · You're hosting" : item.relations.includes("INVITEE") ? " · You're invited" : ""}</p>
            </div>
            {!done && (
              <div className="flex items-center gap-4">
                <Link2 className="size-[18px] shrink-0 text-muted-foreground" />
                <p className="min-w-0 flex-1 truncate text-foreground">{item.meetingSpace.meetingCode}</p>
                <button type="button" onClick={copyLink} className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10">{copied ? <><Check className="size-4" />Copied</> : "Copy link"}</button>
              </div>
            )}
          </div>

          {!item.materialized && !done && <p className="mt-4 rounded-xl bg-muted px-4 py-3 text-sm text-muted-foreground">This repeat isn&apos;t open yet. You can join closer to the time.</p>}

          {actions.join && item.conferenceId && (
            <Link href={`/conferences/${encodeURIComponent(item.conferenceId)}`} className={cn("mt-5 flex h-12 w-full items-center justify-center gap-2 rounded-full text-[15px] font-semibold text-white transition-colors", live ? "bg-emerald-600 hover:bg-emerald-700" : "bg-primary hover:bg-primary/90")}>
              <Video className="size-5" />{live ? "Join now" : "Join meeting"}
            </Link>
          )}
          {download.isError ? <div className="mt-3"><RequestError error={download.error} /></div> : null}
        </div>
      </> : <form className="space-y-4 p-6" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <DialogHeader className="text-left">
          <DialogTitle>{mode.includes("cancel") ? `Cancel ${one}?` : `Change time of ${one}`}</DialogTitle>
          <DialogDescription>{name}</DialogDescription>
        </DialogHeader>
        {mode === "reschedule" || mode === "exception-reschedule" ? <>
          <p className="text-sm text-muted-foreground">{actions.recurring ? "Only this session moves. The rest of the series stays the same. " : ""}Times are in the meeting&apos;s time zone. Leave the end empty to keep the same length.</p>
          <ZonedDateTimeField id="occurrence-start" label="New start" required value={start} onChange={moveStart} timeZone={zone} />
          <ZonedDateTimeField id="occurrence-end" label="New end (optional)" value={end} min={start} onChange={setEnd} timeZone={zone} />
          {start ? <p className="text-xs text-muted-foreground">Your time: {(() => { try { return <ScheduleText start={occurrenceWindowInZone(start, null, zone, 0).scheduledStartAt} timeZone={viewerTimeZone()} />; } catch { return "—"; } })()}</p> : null}
        </> : <p className="text-sm text-muted-foreground">{actions.recurring ? "Other sessions in the series stay as they are. " : ""}People won&apos;t be able to join it anymore.</p>}
        {error !== undefined ? <RequestError error={error} /> : null}
        <DialogFooter className="gap-2">
          <Button type="button" variant="outline" className="rounded-full" disabled={pending} onClick={() => { setMode("view"); setError(undefined); }}>Back</Button>
          <Button type="submit" variant={mode.includes("cancel") ? "destructive" : "default"} className="rounded-full" disabled={pending}>{pending ? "Saving…" : mode.includes("cancel") ? `Cancel ${one}` : "Save new time"}</Button>
        </DialogFooter>
      </form>}
    </DialogContent>
  </Dialog>;
}
