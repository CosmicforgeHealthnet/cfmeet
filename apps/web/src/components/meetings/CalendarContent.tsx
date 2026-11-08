"use client";

/**
 * Personal / organization calendar (Blueprint Section 10 / 17; MTG-005 client, ORG-011, MTG-012).
 * Day / week / month / agenda are views of ONE agenda query per visible range:
 * `GET /api/v2/me/conferences?from&to&status&role&organizationId&cursor&limit`.
 */
import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ChevronLeft, ChevronRight, CalendarDays, Plus, Loader2, Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCreateInstantMeeting } from "@/hooks/v2/useSpaces";
import { useAgendaRange, useCalendarDownload, useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import {
  AGENDA_RELATIONS,
  agendaItemKey,
  datesBetween,
  dayPlacement,
  groupByLocalDate,
  shiftAnchor,
  todayIn,
  viewRange,
  type CalendarView,
} from "@/lib/meeting-calendar";
import { formatTimeInZone, viewerTimeZone } from "@/lib/meeting-time";
import { getErrorMessage } from "@/utils/error";
import type { AgendaItem } from "@/types/generated/meeting-service";
import OccurrenceDialog from "./OccurrenceDialog";
import { ViewerZoneNote } from "./ScheduleFields";

const VIEWS: { value: CalendarView; label: string }[] = [
  { value: "day", label: "Day" },
  { value: "week", label: "Week" },
  { value: "month", label: "Month" },
  { value: "agenda", label: "Agenda" },
];

const STATUS_FILTERS = [
  { value: "CREATED,PREPARING,WAITING,LIVE", label: "Upcoming & live" },
  { value: "", label: "All" },
  { value: "ENDED", label: "Ended" },
  { value: "CANCELLED", label: "Cancelled" },
];

const RELATION_LABELS: Record<string, string> = {
  OWNER: "I own", MEMBER: "Member", INVITEE: "Invited", REGISTRANT: "Registered", PARTICIPANT: "Attended", ORGANIZATION: "Organization",
};

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function statusTone(status: AgendaItem["status"]) {
  if (status === "LIVE") return "border-emerald-500/50 bg-emerald-500/10";
  if (status === "CANCELLED" || status === "FAILED") return "border-border bg-muted/40 line-through opacity-70";
  if (status === "ENDED" || status === "ENDING") return "border-border bg-muted/40";
  return "border-primary/40 bg-primary/5";
}

function title(item: AgendaItem) {
  return item.meetingSpace.title || "CF Meet meeting";
}

function dateHeading(date: string, options: Intl.DateTimeFormatOptions) {
  return new Intl.DateTimeFormat("en-US", { ...options, timeZone: "UTC" }).format(new Date(`${date}T12:00:00Z`));
}

/** Side-by-side columns for overlapping occurrences of one day. */
function dayColumns(list: AgendaItem[], zone: string) {
  const placed = list.map((item) => ({ item, place: dayPlacement(item, zone), column: 0, columns: 1 }));
  const ends: number[] = [];
  for (const entry of placed) {
    const start = entry.place.startMinute;
    let column = ends.findIndex((end) => end <= start);
    if (column < 0) column = ends.length;
    ends[column] = start + entry.place.durationMinutes;
    entry.column = column;
  }
  const columns = Math.max(1, ends.length);
  for (const entry of placed) entry.columns = columns;
  return placed;
}

export default function CalendarContent() {
  const router = useRouter();
  const zone = viewerTimeZone();
  const capabilities = useMeetingCapabilities();
  const [view, setView] = useState<CalendarView>("week");
  const [anchor, setAnchor] = useState(() => todayIn(zone));
  const [status, setStatus] = useState(STATUS_FILTERS[0].value);
  const [relation, setRelation] = useState("");
  const [scope, setScope] = useState<"organization" | "mine">("organization");
  const [selected, setSelected] = useState<AgendaItem | null>(null);
  const [instantError, setInstantError] = useState("");
  const instant = useCreateInstantMeeting();
  const download = useCalendarDownload();

  const range = useMemo(() => viewRange(view, anchor, zone), [view, anchor, zone]);
  const organizationId = scope === "organization" ? capabilities.organizationId : null;
  const query = useMemo(() => ({
    from: range.from,
    to: range.to,
    ...(status ? { status } : {}),
    ...(relation ? { role: relation } : {}),
    ...(organizationId ? { organizationId } : {}),
  }), [range.from, range.to, status, relation, organizationId]);
  const agenda = useAgendaRange(query);
  const items = useMemo(() => agenda.data?.items ?? [], [agenda.data]);
  const byDate = useMemo(() => groupByLocalDate(items, zone), [items, zone]);
  const forbidden = (agenda.error as { response?: { status?: number } } | null)?.response?.status === 403;
  const canSchedule = capabilities.organizationId ? capabilities.canCreateOrganization || capabilities.canCreatePersonal : capabilities.canCreatePersonal;
  const current = selected ? items.find((item) => agendaItemKey(item) === agendaItemKey(selected)) ?? selected : null;

  const startInstant = async () => {
    setInstantError("");
    try {
      const created = await instant.mutateAsync({ timezone: zone });
      router.push(`/conferences/${encodeURIComponent(created.conference.id)}`);
    } catch (cause) {
      setInstantError(getErrorMessage(cause, "Could not start an instant meeting."));
    }
  };

  const heading = view === "day"
    ? dateHeading(anchor, { weekday: "long", month: "long", day: "numeric", year: "numeric" })
    : view === "month"
      ? dateHeading(`${anchor.slice(0, 7)}-15`, { month: "long", year: "numeric" })
      : `${dateHeading(range.start, { month: "short", day: "numeric" })} – ${dateHeading(datesBetween(range.start, range.end).at(-1) ?? range.start, { month: "short", day: "numeric", year: "numeric" })}`;

  const entry = (item: AgendaItem, compact = false) => (
    <button key={agendaItemKey(item)} type="button" onClick={() => setSelected(item)} className={`w-full rounded-md border px-2 py-1 text-left text-xs transition hover:shadow-sm ${statusTone(item.status)}`}>
      <span className="font-medium">{formatTimeInZone(item.scheduledStartAt, zone).replace(/ [A-Z]{2,5}$|\s?GMT[+-]\d+$/, "")}</span>{" "}
      <span className={compact ? "line-clamp-1" : ""}>{title(item)}</span>
      {item.status === "LIVE" ? <span className="ml-1 rounded bg-emerald-600 px-1 text-[10px] text-white">LIVE</span> : null}
      {!item.materialized ? <span className="ml-1 text-[10px] text-muted-foreground">(series)</span> : null}
    </button>
  );

  return (
    <div className="mx-auto max-w-7xl space-y-5 px-4 py-6 sm:px-6 md:px-10">
      <header className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <h1 className="text-2xl font-bold">{organizationId ? `${capabilities.organizationName ?? "Organization"} calendar` : "My calendar"}</h1>
          <p className="text-sm text-muted-foreground" suppressHydrationWarning>Times in your timezone: <ViewerZoneNote />. Each meeting also shows its own timezone.</p>
        </div>
        <div className="flex flex-wrap gap-2">
          {canSchedule ? <Button asChild><Link href="/dashboard/meetings/new"><CalendarDays className="mr-1 size-4" />Schedule meeting</Link></Button> : null}
          {capabilities.canCreatePersonal ? <Button variant="outline" onClick={() => void startInstant()} disabled={instant.isPending} aria-busy={instant.isPending}>{instant.isPending ? <Loader2 className="mr-1 size-4 animate-spin" /> : <Plus className="mr-1 size-4" />}Instant meeting</Button> : null}
        </div>
      </header>
      {instantError ? <p role="alert" className="text-sm text-destructive">{instantError}</p> : null}

      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Button variant="outline" size="icon" aria-label="Previous" onClick={() => setAnchor(shiftAnchor(view, anchor, -1))}><ChevronLeft className="size-4" /></Button>
          <Button variant="outline" onClick={() => setAnchor(todayIn(zone))}>Today</Button>
          <Button variant="outline" size="icon" aria-label="Next" onClick={() => setAnchor(shiftAnchor(view, anchor, 1))}><ChevronRight className="size-4" /></Button>
          <h2 className="ml-2 text-lg font-semibold" suppressHydrationWarning>{heading}</h2>
          {agenda.isFetching ? <Loader2 className="size-4 animate-spin text-muted-foreground" aria-label="Loading" /> : null}
        </div>
        <div className="flex flex-wrap items-center gap-2" role="tablist" aria-label="Calendar view">
          {VIEWS.map((option) => <Button key={option.value} role="tab" aria-selected={view === option.value} size="sm" variant={view === option.value ? "default" : "outline"} onClick={() => setView(option.value)}>{option.label}</Button>)}
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-3 text-sm">
        {capabilities.organizationId ? <label className="flex items-center gap-2">Calendar
          <select className="h-9 rounded-md border border-input bg-background px-2" value={scope} onChange={(event) => setScope(event.target.value as typeof scope)}>
            <option value="organization">{capabilities.organizationName ?? "Organization"} meetings</option>
            <option value="mine">All my meetings</option>
          </select></label> : null}
        <label className="flex items-center gap-2">Status
          <select className="h-9 rounded-md border border-input bg-background px-2" value={status} onChange={(event) => setStatus(event.target.value)}>
            {STATUS_FILTERS.map((option) => <option key={option.label} value={option.value}>{option.label}</option>)}
          </select></label>
        <label className="flex items-center gap-2">Relation
          <select className="h-9 rounded-md border border-input bg-background px-2" value={relation} onChange={(event) => setRelation(event.target.value)}>
            <option value="">Any</option>
            {AGENDA_RELATIONS.filter((value) => value !== "ORGANIZATION" || organizationId).map((value) => <option key={value} value={value}>{RELATION_LABELS[value]}</option>)}
          </select></label>
      </div>

      {forbidden && organizationId ? <p role="alert" className="rounded-lg border border-border bg-muted p-3 text-sm">The organization calendar is not available to you in this workspace.</p> : null}
      {agenda.isError && !forbidden ? <RequestError error={agenda.error} retry={() => void agenda.refetch()} /> : null}
      {agenda.data?.truncated ? <p className="text-sm text-amber-700 dark:text-amber-400">This range has more occurrences than can be shown at once. Narrow the view or the filters.</p> : null}
      {agenda.isLoading ? <div className="flex justify-center py-16"><Spinner /></div> : null}

      {!agenda.isLoading && !forbidden && view === "month" ? (
        <div className="overflow-x-auto">
          <div className="grid min-w-[720px] grid-cols-7 gap-px overflow-hidden rounded-xl border border-border bg-border">
            {WEEKDAYS.map((day) => <div key={day} className="bg-muted px-2 py-1 text-xs font-medium">{day}</div>)}
            {datesBetween(range.start, range.end).map((date) => {
              const list = byDate.get(date) ?? [];
              const outside = date.slice(0, 7) !== anchor.slice(0, 7);
              return <div key={date} className={`min-h-28 space-y-1 bg-card p-1.5 ${outside ? "opacity-50" : ""}`}>
                <button type="button" className={`text-xs ${date === todayIn(zone) ? "rounded-full bg-primary px-1.5 text-white" : "text-muted-foreground"}`} onClick={() => { setAnchor(date); setView("day"); }}>{Number(date.slice(8))}</button>
                {list.slice(0, 3).map((item) => entry(item, true))}
                {list.length > 3 ? <button type="button" className="text-xs text-primary" onClick={() => { setAnchor(date); setView("day"); }}>+{list.length - 3} more</button> : null}
              </div>;
            })}
          </div>
        </div>
      ) : null}

      {!agenda.isLoading && !forbidden && view === "week" ? (
        <div className="overflow-x-auto">
          <div className="grid min-w-[840px] grid-cols-7 gap-2">
            {datesBetween(range.start, range.end).map((date) => <section key={date} className="min-h-48 space-y-1 rounded-xl border border-border bg-card p-2">
              <button type="button" className={`mb-1 text-xs font-medium ${date === todayIn(zone) ? "text-primary" : "text-muted-foreground"}`} onClick={() => { setAnchor(date); setView("day"); }}>{dateHeading(date, { weekday: "short", day: "numeric" })}</button>
              {(byDate.get(date) ?? []).map((item) => entry(item))}
            </section>)}
          </div>
        </div>
      ) : null}

      {!agenda.isLoading && !forbidden && view === "day" ? (
        <div className="relative rounded-xl border border-border bg-card" style={{ height: 24 * 48 }}>
          {Array.from({ length: 24 }, (_, hour) => <div key={hour} className="absolute inset-x-0 border-t border-border/60 pl-2 text-[10px] text-muted-foreground" style={{ top: hour * 48 }}>{String(hour).padStart(2, "0")}:00</div>)}
          {dayColumns(byDate.get(anchor) ?? [], zone).map(({ item, place, column, columns }) => (
            <div key={agendaItemKey(item)} className="absolute overflow-hidden pr-1" style={{ top: (place.startMinute / 60) * 48, height: Math.max(22, (place.durationMinutes / 60) * 48), left: `calc(3.5rem + (100% - 4rem) * ${column / columns})`, width: `calc((100% - 4rem) / ${columns})` }}>
              {entry(item)}
            </div>
          ))}
          {(byDate.get(anchor) ?? []).length === 0 ? <p className="absolute inset-x-0 top-1/3 text-center text-sm text-muted-foreground">No meetings on this day.</p> : null}
        </div>
      ) : null}

      {!agenda.isLoading && !forbidden && view === "agenda" ? (
        <div className="space-y-4">
          {items.length === 0 ? <p className="rounded-xl border border-dashed p-10 text-center text-muted-foreground">No meetings in the next {datesBetween(range.start, range.end).length} days.</p> : null}
          {[...byDate.entries()].map(([date, list]) => <section key={date} className="space-y-2">
            <h3 className="text-sm font-semibold text-muted-foreground">{dateHeading(date, { weekday: "long", month: "long", day: "numeric" })}</h3>
            {list.map((item) => <article key={agendaItemKey(item)} className={`flex flex-wrap items-center justify-between gap-2 rounded-xl border p-3 ${statusTone(item.status)}`}>
              <button type="button" className="min-w-0 flex-1 text-left" onClick={() => setSelected(item)}>
                <p className="font-medium">{title(item)}</p>
                <p className="text-xs text-muted-foreground">{formatTimeInZone(item.scheduledStartAt, zone)}{item.scheduledEndAt ? ` – ${formatTimeInZone(item.scheduledEndAt, zone)}` : ""}{item.meetingSpace.timezone !== zone ? ` · ${formatTimeInZone(item.scheduledStartAt, item.meetingSpace.timezone)} meeting time` : ""}</p>
                <p className="text-xs text-muted-foreground">{item.status}{item.relations.length ? ` · ${item.relations.map((r) => RELATION_LABELS[r] ?? r).join(", ")}` : ""}{!item.materialized ? " · recurring (not yet generated)" : ""}</p>
              </button>
              {item.conferenceId ? <Button size="sm" variant="ghost" aria-label="Add to calendar" disabled={download.isPending} onClick={() => download.mutate({ kind: "conference", id: item.conferenceId as string, name: title(item) })}><Download className="size-4" /></Button> : null}
            </article>)}
          </section>)}
        </div>
      ) : null}
      {download.isError ? <RequestError error={download.error} /> : null}

      {current ? <OccurrenceDialog item={current} onClose={() => setSelected(null)} /> : null}
    </div>
  );
}
