"use client";

/** Dashboard "Upcoming": the next scheduled occurrences from one agenda page (7 days). */
import { useMemo, useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useAgendaPage, useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import { agendaItemKey } from "@/lib/meeting-calendar";
import { getErrorMessage } from "@/utils/error";
import type { AgendaItem } from "@/types/generated/meeting-service";
import { ScheduleText } from "./ScheduleFields";
import OccurrenceDialog from "./OccurrenceDialog";

export default function UpcomingMeetings() {
  const { organizationId } = useMeetingCapabilities();
  // Rounded to the minute so the query key is stable across renders.
  const [from] = useState(() => new Date(Math.floor(Date.now() / 60_000) * 60_000 - 60 * 60_000).toISOString());
  const query = useMemo(() => ({
    from,
    to: new Date(Date.parse(from) + 7 * 86_400_000).toISOString(),
    status: "CREATED,PREPARING,WAITING,LIVE",
    limit: 5,
    ...(organizationId ? { organizationId } : {}),
  }), [from, organizationId]);
  const agenda = useAgendaPage(query);
  const [selected, setSelected] = useState<AgendaItem | null>(null);
  const forbidden = (agenda.error as { response?: { status?: number } } | null)?.response?.status === 403;
  return <section className="mt-10 px-0 sm:px-4 md:px-20">
    <div className="mb-4 flex items-center justify-between gap-3">
      <h3 className="text-base font-semibold sm:text-lg">Upcoming{organizationId ? " in this organization" : ""}</h3>
      <Button variant="outline" size="sm" asChild><Link href="/dashboard/calendar">Open calendar</Link></Button>
    </div>
    {agenda.isLoading ? <Spinner /> : forbidden ? <p className="text-sm text-muted-foreground">The organization calendar is not available to you.</p> : agenda.isError ? <p role="alert" className="text-sm text-destructive">{getErrorMessage(agenda.error, "Could not load upcoming meetings.")}</p> : (agenda.data?.items.length ?? 0) === 0 ? <p className="text-sm text-muted-foreground">No meetings scheduled in the next 7 days.</p> : <ul className="space-y-2">
      {agenda.data?.items.map((item) => <li key={agendaItemKey(item)}>
        <button type="button" onClick={() => setSelected(item)} className="flex w-full flex-wrap items-center justify-between gap-2 rounded-xl border border-border bg-card p-3 text-left text-sm hover:border-primary/50">
          <span className="min-w-0"><span className="block font-medium">{item.meetingSpace.title || "CF Meet meeting"}</span><ScheduleText start={item.scheduledStartAt} end={item.scheduledEndAt} timeZone={item.meetingSpace.timezone} className="text-xs text-muted-foreground" /></span>
          {item.status === "LIVE" ? <span className="rounded-full bg-emerald-600 px-2 py-0.5 text-xs text-white">Live</span> : null}
        </button>
      </li>)}
    </ul>}
    {selected ? <OccurrenceDialog item={selected} onClose={() => setSelected(null)} /> : null}
  </section>;
}
