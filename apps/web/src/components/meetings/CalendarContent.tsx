"use client";

/**
 * Personal / organization calendar (Blueprint Section 10 / 17; MTG-005 client, ORG-011, MTG-012).
 * Day / week / month / list are views of ONE agenda query per visible range:
 * `GET /api/v2/me/conferences?from&to&status&role&organizationId&cursor&limit`.
 *
 * Layout: one full-height surface that never scrolls as a page (Google Calendar / Outlook);
 * only the time grid or list scrolls inside it.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { Popover } from "radix-ui";
import { CalendarDays, CalendarPlus, Check, ChevronDown, ChevronLeft, ChevronRight, Download, ListFilter, Plus, Repeat, Video } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { CalendarArt } from "@/components/dashboard/EmptyArt";
import { useAgendaRange, useCalendarDownload, useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import { conferencesService } from "@/services/v2/conferences.service";
import {
  AGENDA_RELATIONS,
  agendaItemKey,
  datesBetween,
  dayPlacement,
  groupByLocalDate,
  shiftAnchor,
  todayIn,
  viewDates,
  viewRange,
  type CalendarView,
} from "@/lib/meeting-calendar";
import { viewerTimeZone } from "@/lib/meeting-time";
import { cn } from "@/lib/utils";
import type { AgendaItem } from "@/types/generated/meeting-service";
import OccurrenceDialog from "./OccurrenceDialog";

const HOUR = 56; // px per hour in the time grid

const STATUS_FILTERS = [
  { value: "CREATED,PREPARING,WAITING,LIVE", label: "Upcoming and live" },
  { value: "", label: "Everything" },
  { value: "ENDED", label: "Ended" },
  { value: "CANCELLED", label: "Cancelled" },
];

const RELATION_LABELS: Record<string, string> = {
  OWNER: "I'm hosting", MEMBER: "I'm a member", INVITEE: "I'm invited", REGISTRANT: "I registered", PARTICIPANT: "I attended", ORGANIZATION: "My organization",
};

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** Tinted blocks (Notion Calendar / Vimcal): soft fill, darker same-hue text, hairline edge. */
function block(status: AgendaItem["status"]) {
  if (status === "LIVE") return "bg-[#e6f6ee] text-[#0b5a36] ring-[#b7e4cc] hover:bg-[#d9f1e5] dark:bg-emerald-500/20 dark:text-emerald-50 dark:ring-emerald-400/30";
  if (status === "CANCELLED" || status === "FAILED") return "bg-transparent text-muted-foreground line-through ring-border";
  if (status === "ENDED" || status === "ENDING") return "bg-muted/70 text-muted-foreground ring-transparent hover:bg-muted";
  return "bg-[#e8f0fe] text-[#174ea6] ring-[#cfe0fc] hover:bg-[#dde8fd] dark:bg-primary/20 dark:text-blue-50 dark:ring-primary/30";
}
const dot = (status: AgendaItem["status"]) => status === "LIVE" ? "bg-emerald-500" : ["CANCELLED", "ENDED", "ENDING", "FAILED"].includes(status) ? "bg-muted-foreground/40" : "bg-primary";

const name = (item: AgendaItem) => item.meetingSpace.title || "CF Meet meeting";
const fmtDate = (date: string, options: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(undefined, { ...options, timeZone: "UTC" }).format(new Date(`${date}T12:00:00Z`));
const fmtTime = (iso: string, zone: string) => new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: zone }).format(new Date(iso)).replace(":00", "");
/** "4:05 – 7:30 PM": the shared AM/PM is written once so it fits narrow columns. */
const fmtSpan = (start: string, end: string | null | undefined, zone: string) => {
  const a = fmtTime(start, zone);
  if (!end) return a;
  const b = fmtTime(end, zone);
  const ma = a.match(/\s?([AaPp]\.?[Mm]\.?)$/)?.[1], mb = b.match(/\s?([AaPp]\.?[Mm]\.?)$/)?.[1];
  return ma && ma === mb ? `${a.replace(/\s?[AaPp]\.?[Mm]\.?$/, "")} – ${b}` : `${a} – ${b}`;
};
const hourLabel = (h: number) => new Intl.DateTimeFormat(undefined, { hour: "numeric", timeZone: "UTC" }).format(new Date(Date.UTC(2020, 0, 1, h)));

function nowMinute(zone: string) {
  const parts = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: zone }).formatToParts(new Date());
  return Number(parts.find((p) => p.type === "hour")?.value ?? 0) * 60 + Number(parts.find((p) => p.type === "minute")?.value ?? 0);
}

/** Side-by-side columns only within each group of overlapping meetings; others keep full width. */
function dayColumns(list: AgendaItem[], zone: string) {
  const placed = list
    .map((item) => ({ item, place: dayPlacement(item, zone), column: 0, columns: 1 }))
    .sort((a, b) => a.place.startMinute - b.place.startMinute);
  let cluster: typeof placed = [];
  let ends: number[] = [];
  let clusterEnd = -1;
  const close = () => { for (const e of cluster) e.columns = Math.max(1, ends.length); cluster = []; ends = []; };
  for (const entry of placed) {
    const start = entry.place.startMinute;
    const end = start + Math.max(entry.place.durationMinutes, 20);
    if (start >= clusterEnd) close();
    let column = ends.findIndex((e) => e <= start);
    if (column < 0) column = ends.length;
    ends[column] = end;
    entry.column = column;
    cluster.push(entry);
    clusterEnd = Math.max(clusterEnd, end);
  }
  close();
  return placed;
}

function useIsPhone() {
  const [phone, setPhone] = useState(false);
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 767px)");
    const update = () => setPhone(mq.matches);
    update();
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, []);
  return phone;
}

export default function CalendarContent({ previewItems, previewOpen }: { previewItems?: AgendaItem[]; previewOpen?: boolean } = {}) {
  const zone = viewerTimeZone();
  const phone = useIsPhone();
  const capabilities = useMeetingCapabilities();
  const [view, setView] = useState<CalendarView>("week");
  const [anchor, setAnchor] = useState(() => todayIn(zone));
  const [status, setStatus] = useState(STATUS_FILTERS[0].value);
  const [relation, setRelation] = useState("");
  const [scope, setScope] = useState<"organization" | "mine">("organization");
  const [selected, setSelected] = useState<AgendaItem | null>(() => (previewOpen && previewItems?.[0]) || null);
  const download = useCalendarDownload();
  const today = todayIn(zone);

  // Phones: 7 columns are unreadable, so Week shows as Day with a week strip (Google Calendar app).
  const shown: CalendarView = phone && view === "week" ? "day" : view;
  const range = useMemo(() => viewRange(shown, anchor, zone), [shown, anchor, zone]);
  const organizationId = scope === "organization" ? capabilities.organizationId : null;
  const query = useMemo(() => ({
    from: range.from,
    to: range.to,
    ...(status ? { status } : {}),
    ...(relation ? { role: relation } : {}),
    ...(organizationId ? { organizationId } : {}),
  }), [range.from, range.to, status, relation, organizationId]);
  const agenda = useAgendaRange(query);
  const items = useMemo(() => previewItems ?? agenda.data?.items ?? [], [previewItems, agenda.data]);
  const byDate = useMemo(() => groupByLocalDate(items, zone), [items, zone]);
  const forbidden = (agenda.error as { response?: { status?: number } } | null)?.response?.status === 403;
  const canSchedule = capabilities.organizationId ? capabilities.canCreateOrganization || capabilities.canCreatePersonal : capabilities.canCreatePersonal;
  const current = selected ? items.find((item) => agendaItemKey(item) === agendaItemKey(selected)) ?? selected : null;
  const filtersOn = (status !== STATUS_FILTERS[0].value ? 1 : 0) + (relation ? 1 : 0) + (capabilities.organizationId && scope === "mine" ? 1 : 0);

  const lastDay = datesBetween(range.start, range.end).at(-1) ?? range.start;
  const heading = shown === "day" && !phone
    ? fmtDate(anchor, { weekday: "short", month: "short", day: "numeric" })
    : shown === "agenda"
      ? `${fmtDate(range.start, { month: "short", day: "numeric" })} – ${fmtDate(lastDay, { month: "short", day: "numeric" })}`
      : shown === "week" && range.start.slice(0, 7) !== lastDay.slice(0, 7)
        ? `${fmtDate(range.start, { month: "short" })} – ${fmtDate(lastDay, { month: "short", year: "numeric" })}`
        : fmtDate(shown === "month" ? `${anchor.slice(0, 7)}-15` : anchor, phone && anchor.slice(0, 4) === today.slice(0, 4) ? { month: "long" } : { month: "long", year: "numeric" });

  const views: { value: CalendarView; label: string }[] = [
    { value: "day", label: "Day" },
    ...(phone ? [] : [{ value: "week" as const, label: "Week" }]),
    { value: "month", label: "Month" },
    { value: "agenda", label: "List" },
  ];
  const pickDay = (date: string) => { setAnchor(date); setView("day"); };
  const iconBtn = "flex size-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground";

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background">
      {/* Toolbar */}
      <div className="flex shrink-0 items-center gap-1 px-3 py-3 sm:gap-2 sm:px-6">
        <button type="button" onClick={() => setAnchor(today)} aria-label="Go to today" className="h-9 shrink-0 rounded-lg border border-border px-3.5 text-sm font-medium text-foreground hover:bg-muted max-sm:hidden">Today</button>
        <button type="button" onClick={() => setAnchor(today)} aria-label="Go to today" className={cn(iconBtn, "sm:hidden")}><CalendarDays className="size-5" /></button>
        <button type="button" aria-label="Previous" onClick={() => setAnchor(shiftAnchor(shown, anchor, -1))} className={cn(iconBtn, "max-sm:hidden")}><ChevronLeft className="size-5" /></button>
        <button type="button" aria-label="Next" onClick={() => setAnchor(shiftAnchor(shown, anchor, 1))} className={cn(iconBtn, "max-sm:hidden")}><ChevronRight className="size-5" /></button>

        <Popover.Root>
          <Popover.Trigger asChild>
            <button type="button" className="flex min-w-0 items-center gap-1 rounded-lg px-2 py-1 text-left hover:bg-muted" suppressHydrationWarning>
              <span className="truncate text-lg font-semibold tracking-tight text-foreground sm:text-xl">{heading}</span>
              <ChevronDown className="size-4 shrink-0 text-muted-foreground" />
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content align="start" sideOffset={6} collisionPadding={12} className="z-50 w-72 rounded-2xl border border-border bg-popover p-3 shadow-xl">
              <MiniMonth anchor={anchor} today={today} onPick={(d) => setAnchor(d)} />
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>

        <NextUp items={items} zone={zone} onOpen={setSelected} visible={(item) => { const d = item.scheduledStartAt; return d >= range.from && d < range.to; }} />
        <div className="ml-auto flex items-center gap-1 sm:gap-2">
          <CalendarFeedControl buttonClass={iconBtn} />
          <Popover.Root>
            <Popover.Trigger asChild>
              <button type="button" aria-label="Filters" className={cn(iconBtn, "relative")}>
                <ListFilter className="size-5" />
                {filtersOn > 0 && <span className="absolute right-1 top-1 flex size-4 items-center justify-center rounded-full bg-primary text-[10px] font-bold text-primary-foreground">{filtersOn}</span>}
              </button>
            </Popover.Trigger>
            <Popover.Portal>
              <Popover.Content align="end" sideOffset={6} collisionPadding={12} className="z-50 w-[min(92vw,20rem)] space-y-4 rounded-2xl border border-border bg-popover p-4 shadow-xl">
                {capabilities.organizationId ? <NiceSelect<"organization" | "mine"> label="Calendar" value={scope} onChange={setScope} options={[{ value: "organization", label: capabilities.organizationName ?? "Organization" }, { value: "mine", label: "All my meetings" }]} /> : null}
                <NiceSelect label="Show" value={status} onChange={setStatus} options={STATUS_FILTERS} />
                <NiceSelect label="Meetings where" value={relation} onChange={setRelation} options={[{ value: "", label: "Any role" }, ...AGENDA_RELATIONS.filter((v) => v !== "ORGANIZATION" || organizationId).map((v) => ({ value: v, label: RELATION_LABELS[v] }))]} />
                <p className="text-xs text-muted-foreground">Times shown in {zone.replaceAll("_", " ")}</p>
              </Popover.Content>
            </Popover.Portal>
          </Popover.Root>
          <Popover.Root>
            <Popover.Trigger asChild>
              <button type="button" className="flex h-9 items-center gap-1 rounded-lg border border-border px-3 text-sm font-medium text-foreground hover:bg-muted md:hidden">{views.find((v) => v.value === shown)?.label}<ChevronDown className="size-4 text-muted-foreground" /></button>
            </Popover.Trigger>
            <Popover.Portal>
              <Popover.Content align="end" sideOffset={6} className="z-50 w-40 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                {views.map((option) => <Popover.Close asChild key={option.value}><button type="button" onClick={() => setView(option.value)} className={cn("flex w-full items-center rounded-lg px-3 py-2.5 text-sm hover:bg-muted", shown === option.value ? "font-semibold text-primary" : "text-foreground")}>{option.label}</button></Popover.Close>)}
              </Popover.Content>
            </Popover.Portal>
          </Popover.Root>
          <div role="tablist" aria-label="Calendar view" className="flex rounded-lg bg-muted p-0.5 max-md:hidden">
            {views.map((option) => (
              <button key={option.value} role="tab" type="button" aria-selected={shown === option.value} onClick={() => setView(option.value)}
                className={cn("h-8 rounded-md px-3 text-sm font-medium transition-colors", shown === option.value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>{option.label}</button>
            ))}
          </div>
          {canSchedule && <Link href="/dashboard/meetings/new" className="hidden h-9 items-center gap-1.5 rounded-lg bg-primary pl-2.5 pr-3.5 text-sm font-semibold text-primary-foreground hover:bg-primary/90 md:inline-flex"><Plus className="size-4" />Schedule</Link>}
        </div>
      </div>

      {(forbidden && organizationId) || agenda.isError || agenda.data?.truncated || download.isError ? (
        <div className="space-y-2 px-3 pb-2 sm:px-6">
          {forbidden && organizationId ? <p role="alert" className="rounded-xl bg-muted px-4 py-3 text-sm">You can&apos;t see this organization&apos;s calendar. Choose &quot;All my meetings&quot; in filters.</p> : null}
          {agenda.isError && !forbidden ? <RequestError error={agenda.error} retry={() => void agenda.refetch()} /> : null}
          {agenda.data?.truncated ? <p className="rounded-xl bg-amber-500/10 px-4 py-2.5 text-sm text-amber-800 dark:text-amber-300">Some meetings in this range aren&apos;t shown. Pick a shorter view or add a filter.</p> : null}
          {download.isError ? <RequestError error={download.error} /> : null}
        </div>
      ) : null}

      <div className="flex min-h-0 flex-1 flex-col border-t border-border">
        {!forbidden && shown === "week" && <TimeGrid dates={datesBetween(range.start, range.end)} byDate={byDate} zone={zone} today={today} loading={agenda.isLoading} onSelect={setSelected} onPickDay={pickDay} />}
        {!forbidden && shown === "day" && (
          <>
            {phone && <WeekStrip anchor={anchor} today={today} onPick={setAnchor} />}
            <TimeGrid dates={[anchor]} byDate={byDate} zone={zone} today={today} loading={agenda.isLoading} onSelect={setSelected} onPickDay={pickDay} hideHeader={phone} />
          </>
        )}
        {!forbidden && shown === "month" && <MonthGrid range={range} anchor={anchor} byDate={byDate} zone={zone} today={today} onSelect={setSelected} onPickDay={pickDay} />}
        {!forbidden && shown === "agenda" && (
          <ListView byDate={byDate} zone={zone} today={today} loading={agenda.isLoading} onSelect={setSelected} canSchedule={canSchedule}
            onDownload={(item) => item.conferenceId && download.mutate({ kind: "conference", id: item.conferenceId, name: name(item) })} downloading={download.isPending} />
        )}
      </div>

      {canSchedule && <Link href="/dashboard/meetings/new" aria-label="Schedule a meeting" className="fixed bottom-24 right-4 z-30 flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-lg shadow-primary/30 md:hidden"><Plus className="size-6" /></Link>}
      {current ? <OccurrenceDialog item={current} onClose={() => setSelected(null)} /> : null}
    </div>
  );
}

function NextUp({ items, zone, onOpen, visible }: { items: AgendaItem[]; zone: string; onOpen: (item: AgendaItem) => void; visible: (item: AgendaItem) => boolean }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => { const t = setInterval(() => setNow(Date.now()), 30000); return () => clearInterval(t); }, []);
  const next = items
    .filter((item) => item.status === "LIVE" || (["CREATED", "PREPARING", "WAITING"].includes(item.status) && Date.parse(item.scheduledStartAt) > now))
    .sort((a, b) => (a.status === "LIVE" ? -1 : b.status === "LIVE" ? 1 : a.scheduledStartAt.localeCompare(b.scheduledStartAt)))[0];
  // Already in the grid: the card itself is the shortcut, so no duplicate here.
  if (!next || visible(next)) return null;
  const live = next.status === "LIVE";
  const mins = Math.round((Date.parse(next.scheduledStartAt) - now) / 60000);
  const when = live ? "Live now" : mins < 60 ? `in ${Math.max(1, mins)} min` : mins < 24 * 60 ? `at ${fmtTime(next.scheduledStartAt, zone)}` : fmtDate(next.scheduledStartAt.slice(0, 10), { weekday: "short", day: "numeric" });
  return (
    <button type="button" onClick={() => onOpen(next)} className="ml-3 hidden max-w-xs items-center gap-2 rounded-full border border-border py-1 pl-2.5 pr-1 text-sm transition-colors hover:bg-muted lg:flex">
      <span className={cn("size-2 shrink-0 rounded-full", live ? "animate-pulse bg-emerald-500" : "bg-primary")} />
      <span className="truncate font-medium text-foreground">{name(next)}</span>
      <span className="shrink-0 text-muted-foreground">{when}</span>
      <span className={cn("shrink-0 rounded-full px-2.5 py-1 text-xs font-semibold text-white", live ? "bg-emerald-600" : "bg-primary")}>{live ? "Join" : "Open"}</span>
    </button>
  );
}

function MiniMonth({ anchor, today, onPick }: { anchor: string; today: string; onPick: (date: string) => void }) {
  const [month, setMonth] = useState(anchor.slice(0, 7));
  const grid = viewDates("month", `${month}-15`);
  const days = datesBetween(grid.start, grid.end);
  const shift = (d: 1 | -1) => setMonth(shiftAnchor("month", `${month}-15`, d).slice(0, 7));
  return (
    <div>
      <div className="mb-1 flex items-center justify-between">
        <p className="pl-2 text-sm font-semibold text-foreground">{fmtDate(`${month}-15`, { month: "long", year: "numeric" })}</p>
        <div className="flex">
          <button type="button" aria-label="Previous month" onClick={() => shift(-1)} className="flex size-8 items-center justify-center rounded-full hover:bg-muted"><ChevronLeft className="size-4" /></button>
          <button type="button" aria-label="Next month" onClick={() => shift(1)} className="flex size-8 items-center justify-center rounded-full hover:bg-muted"><ChevronRight className="size-4" /></button>
        </div>
      </div>
      <div className="grid grid-cols-7 text-center text-[11px] font-medium text-muted-foreground">{WEEKDAYS.map((d) => <span key={d} className="py-1">{d[0]}</span>)}</div>
      <div className="grid grid-cols-7 text-center">
        {days.map((date) => (
          <Popover.Close asChild key={date}>
            <button type="button" onClick={() => onPick(date)}
              className={cn("mx-auto my-0.5 flex size-8 items-center justify-center rounded-full text-xs tabular-nums transition-colors",
                date === today ? "bg-primary font-semibold text-primary-foreground" : date === anchor ? "bg-primary/15 font-semibold text-primary" : date.slice(0, 7) === month ? "text-foreground hover:bg-muted" : "text-muted-foreground/50 hover:bg-muted")}>
              {Number(date.slice(8))}
            </button>
          </Popover.Close>
        ))}
      </div>
    </div>
  );
}

/** Phone day view: swipeable week of date chips above the grid (Google Calendar app). */
function WeekStrip({ anchor, today, onPick }: { anchor: string; today: string; onPick: (date: string) => void }) {
  const week = viewDates("week", anchor);
  return (
    <div className="flex shrink-0 items-center gap-1 border-b border-border px-1 py-2">
      <button type="button" aria-label="Previous week" onClick={() => onPick(shiftAnchor("week", anchor, -1))} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ChevronLeft className="size-4" /></button>
      <div className="grid flex-1 grid-cols-7">
        {datesBetween(week.start, week.end).map((date) => (
          <button key={date} type="button" onClick={() => onPick(date)} className="flex flex-col items-center gap-1 py-0.5">
            <span className={cn("text-[11px] font-medium", date === today ? "text-primary" : "text-muted-foreground")}>{fmtDate(date, { weekday: "narrow" })}</span>
            <span className={cn("flex size-8 items-center justify-center rounded-full text-sm font-semibold tabular-nums",
              date === anchor ? "bg-primary text-primary-foreground" : date === today ? "text-primary" : "text-foreground")}>{Number(date.slice(8))}</span>
          </button>
        ))}
      </div>
      <button type="button" aria-label="Next week" onClick={() => onPick(shiftAnchor("week", anchor, 1))} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ChevronRight className="size-4" /></button>
    </div>
  );
}

function TimeGrid({ dates, byDate, zone, today, loading, onSelect, onPickDay, hideHeader }: {
  dates: string[]; byDate: Map<string, AgendaItem[]>; zone: string; today: string; loading: boolean;
  onSelect: (item: AgendaItem) => void; onPickDay: (date: string) => void; hideHeader?: boolean;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const [minute, setMinute] = useState(() => nowMinute(zone));
  useEffect(() => { const t = setInterval(() => setMinute(nowMinute(zone)), 60000); return () => clearInterval(t); }, [zone]);
  // Open at the working day (or just before now), never at midnight.
  const key = dates.join();
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = Math.max(0, (dates.includes(today) ? minute / 60 - 1.5 : 8) * HOUR);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  const empty = !loading && dates.every((d) => !(byDate.get(d)?.length));

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {!hideHeader && (
        <div className="flex shrink-0 border-b border-border">
          <div className="w-16 shrink-0" />
          {dates.map((date) => (
            <button key={date} type="button" onClick={() => onPickDay(date)} className="group flex flex-1 items-center justify-center gap-1.5 py-3">
              <span className={cn("text-[13px] font-medium", date === today ? "text-primary" : "text-muted-foreground")}>{fmtDate(date, { weekday: "short" })}</span>
              <span className={cn("flex h-7 min-w-7 items-center justify-center rounded-lg px-1.5 text-[15px] font-semibold tabular-nums transition-colors", date === today ? "bg-primary text-primary-foreground" : "text-foreground group-hover:bg-muted")}>{Number(date.slice(8))}</span>
            </button>
          ))}
        </div>
      )}
      <div ref={scroller} className="no-scrollbar relative min-h-0 flex-1 overflow-y-auto">
        <div className="relative flex" style={{ height: 24 * HOUR }}>
          <div className="relative w-16 shrink-0">
            {Array.from({ length: 23 }, (_, i) => i + 1).map((h) => <span key={h} className={cn("absolute right-3 -translate-y-1/2 text-[11px] tabular-nums text-muted-foreground/80", dates.includes(today) && Math.abs(h * 60 - minute) < 15 && "opacity-0")} style={{ top: h * HOUR }}>{hourLabel(h)}</span>)}
            {dates.includes(today) && <span className="absolute right-1 z-20 -translate-y-1/2 whitespace-nowrap rounded-full bg-red-500 px-1.5 py-0.5 text-[10px] font-semibold leading-none tabular-nums text-white sm:right-1.5 sm:text-[11px]" style={{ top: (minute / 60) * HOUR }}>{new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: zone }).formatToParts(new Date()).filter((p) => p.type !== "dayPeriod").map((p) => p.value).join("").trim()}</span>}
          </div>
          <div className="relative flex flex-1">
            {/* Hour lines span all days, starting just right of the labels (Google). */}
            {Array.from({ length: 23 }, (_, i) => i + 1).map((h) => <span key={h} aria-hidden className="pointer-events-none absolute -left-2 right-0 border-t border-border/60" style={{ top: h * HOUR }} />)}
            {dates.map((date, index) => (
              <div key={date} className={cn("relative flex-1", index > 0 && "border-l border-border/60", dates.length > 1 && [5, 6].includes((new Date(`${date}T12:00:00Z`).getUTCDay() + 6) % 7) && "bg-muted/35 dark:bg-white/[0.025]")}>
                {dayColumns(byDate.get(date) ?? [], zone).map(({ item, place, column, columns }) => {
                  const height = Math.max(24, (place.durationMinutes / 60) * HOUR - 3);
                  const short = height < 50;
                  return (
                    <div key={agendaItemKey(item)} role="button" tabIndex={0} title={`${name(item)} · ${fmtSpan(item.scheduledStartAt, item.scheduledEndAt, zone)}`} onClick={() => onSelect(item)} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onSelect(item); } }}
                      className={cn("absolute z-10 flex cursor-pointer flex-col items-start justify-start overflow-hidden rounded-[6px] px-2 text-left ring-1 ring-inset transition-colors hover:z-10 hover:shadow-[0_4px_12px_rgba(0,0,0,0.08)]", short ? "py-0.5" : "py-1.5", block(item.status))}
                      style={{ top: (place.startMinute / 60) * HOUR + 1, height, left: `calc(${(column / columns) * 100}% + 2px)`, width: `calc(${100 / columns}% - 4px)` }}>
                      {short ? (
                        <p className="w-full truncate text-[13px] leading-5"><span className="font-semibold">{name(item)}</span><span className="opacity-75">, {fmtTime(item.scheduledStartAt, zone)}</span></p>
                      ) : (
                        <>
                          <p className="w-full truncate text-[13px] font-semibold leading-5">{item.status === "LIVE" && <span aria-hidden className="mr-1.5 inline-block size-2 animate-pulse rounded-full bg-emerald-500 align-[1px]" />}{name(item)}</p>
                          <p className="w-full truncate text-xs leading-5 tabular-nums opacity-75">{item.status === "LIVE" ? "Live · " : ""}{fmtSpan(item.scheduledStartAt, item.scheduledEndAt, zone)}</p>
                        </>
                      )}
                    </div>
                  );
                })}
                {date === today && (
                  <div aria-hidden className="pointer-events-none absolute inset-x-0 z-[5] flex items-center" style={{ top: (minute / 60) * HOUR }}>
                    <span className="h-0.5 flex-1 bg-red-500" />
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      </div>
      {empty && (
        <div className="pointer-events-none relative">
          <p className="absolute bottom-6 left-1/2 -translate-x-1/2 whitespace-nowrap rounded-full bg-foreground px-4 py-2 text-sm font-medium text-background shadow-lg">No meetings {dates.length === 1 ? "this day" : "this week"}</p>
        </div>
      )}
    </div>
  );
}

function MonthGrid({ range, anchor, byDate, zone, today, onSelect, onPickDay }: {
  range: { start: string; end: string }; anchor: string; byDate: Map<string, AgendaItem[]>; zone: string; today: string;
  onSelect: (item: AgendaItem) => void; onPickDay: (date: string) => void;
}) {
  const days = datesBetween(range.start, range.end);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="grid shrink-0 grid-cols-7 border-b border-border">{WEEKDAYS.map((d) => <span key={d} className="py-2 text-center text-[11px] font-semibold uppercase tracking-wider text-muted-foreground"><span className="sm:hidden">{d[0]}</span><span className="max-sm:hidden">{d}</span></span>)}</div>
      <div className="grid min-h-0 flex-1 grid-cols-7" style={{ gridTemplateRows: `repeat(${days.length / 7}, minmax(0, 1fr))` }}>
        {days.map((date, i) => {
          const list = byDate.get(date) ?? [];
          const outside = date.slice(0, 7) !== anchor.slice(0, 7);
          return (
            <div key={date} onClick={() => onPickDay(date)} className={cn("min-h-0 cursor-pointer overflow-hidden border-border/70 p-1 hover:bg-muted/40", i % 7 !== 0 && "border-l", i >= 7 && "border-t")}>
              <span className={cn("mx-auto flex size-7 items-center justify-center rounded-full text-xs font-medium tabular-nums", date === today ? "bg-primary text-primary-foreground" : outside ? "text-muted-foreground/50" : "text-foreground")}>{Number(date.slice(8))}</span>
              {list.length > 0 && <div className="mt-1 flex justify-center gap-0.5 sm:hidden">{list.slice(0, 3).map((item) => <span key={agendaItemKey(item)} className={cn("size-1.5 rounded-full", dot(item.status))} />)}</div>}
              <div className="mt-0.5 hidden space-y-0.5 sm:block">
                {list.slice(0, 2).map((item) => (
                  <button key={agendaItemKey(item)} type="button" onClick={(e) => { e.stopPropagation(); onSelect(item); }} className={cn("block w-full truncate rounded px-1.5 py-0.5 text-left text-xs font-medium", block(item.status))}>
                    {fmtTime(item.scheduledStartAt, zone)} {name(item)}
                  </button>
                ))}
                {list.length > 2 && <span className="block px-1.5 text-xs font-semibold text-muted-foreground">{list.length - 2} more</span>}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function ListView({ byDate, zone, today, loading, onSelect, onDownload, downloading, canSchedule }: {
  byDate: Map<string, AgendaItem[]>; zone: string; today: string; loading: boolean;
  onSelect: (item: AgendaItem) => void; onDownload: (item: AgendaItem) => void; downloading: boolean; canSchedule: boolean;
}) {
  if (loading) return <div className="space-y-3 p-4 sm:p-6">{[0, 1, 2].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</div>;
  if (!byDate.size) return (
    <div className="flex flex-1 flex-col items-center justify-center px-6 py-14 text-center">
      <CalendarArt className="size-20" />
      <p className="mt-4 text-lg font-semibold text-foreground">Nothing in the next 31 days</p>
      <p className="mt-1 max-w-sm text-sm text-muted-foreground">Meetings you host, join or are invited to show up here.</p>
      {canSchedule && <Link href="/dashboard/meetings/new" className="mt-5 inline-flex h-11 items-center gap-1.5 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />Schedule a meeting</Link>}
    </div>
  );
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-4xl divide-y divide-border/70">
        {[...byDate.entries()].map(([date, list]) => (
          <section key={date} className="flex gap-3 px-3 py-3 sm:gap-6 sm:px-6">
            <div className="w-12 shrink-0 pt-1 text-center sm:w-14">
              <p className={cn("text-[11px] font-semibold uppercase tracking-wider", date === today ? "text-primary" : "text-muted-foreground")}>{fmtDate(date, { weekday: "short" })}</p>
              <p className={cn("mx-auto mt-0.5 flex size-10 items-center justify-center rounded-full text-xl font-medium tabular-nums", date === today ? "bg-primary text-primary-foreground" : "text-foreground")}>{Number(date.slice(8))}</p>
            </div>
            <ul className="min-w-0 flex-1 space-y-1 pt-1">
              {list.map((item) => (
                <li key={agendaItemKey(item)} className="group flex items-center gap-1 rounded-xl hover:bg-muted/60">
                  <button type="button" onClick={() => onSelect(item)} className="flex min-w-0 flex-1 items-center gap-3 px-2 py-2.5 text-left">
                    <span className={cn("size-2.5 shrink-0 rounded-full", dot(item.status))} />
                    <span className="w-32 shrink-0 text-sm tabular-nums text-muted-foreground max-sm:hidden">{fmtTime(item.scheduledStartAt, zone)}{item.scheduledEndAt ? ` – ${fmtTime(item.scheduledEndAt, zone)}` : ""}</span>
                    <span className="min-w-0 flex-1">
                      <span className={cn("flex items-center gap-1.5 text-sm font-semibold text-foreground", item.status === "CANCELLED" && "text-muted-foreground line-through")}>
                        <span className="truncate">{name(item)}</span>
                        {!item.materialized && <Repeat className="size-3.5 shrink-0 text-muted-foreground" aria-label="Repeats" />}
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        <span className="sm:hidden">{fmtTime(item.scheduledStartAt, zone)} · </span>
                        {item.status === "LIVE" ? <span className="font-semibold text-emerald-600 dark:text-emerald-400">Live now</span> : item.status === "CANCELLED" ? "Cancelled" : item.status === "ENDED" ? "Ended" : item.relations.includes("OWNER") ? "You're hosting" : "You're invited"}
                      </span>
                    </span>
                  </button>
                  {item.conferenceId && item.status !== "CANCELLED" && (
                    <button type="button" aria-label="Add to my calendar" title="Add to my calendar" disabled={downloading} onClick={() => onDownload(item)} className="mr-1 flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground opacity-0 hover:bg-background hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100 max-sm:hidden"><Download className="size-4" /></button>
                  )}
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}

/**
 * Audit MF2: see your CF Meet meetings in Google, Outlook or Apple Calendar through a private
 * link. One tap makes the link and copies it; a new link turns the old one off.
 */
function CalendarFeedControl({ buttonClass }: { buttonClass: string }) {
  const [state, setState] = useState<"idle" | "busy" | "copied" | "off" | "error">("idle");
  const apiOrigin = (() => {
    try { return new URL(process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1").origin; } catch { return ""; }
  })();
  const copy = async () => {
    setState("busy");
    try {
      const feed = await conferencesService.createCalendarFeed();
      if (!feed.path) throw new Error("no link");
      await navigator.clipboard.writeText(`${apiOrigin}${feed.path}`);
      setState("copied");
    } catch { setState("error"); }
  };
  const turnOff = async () => {
    setState("busy");
    try { await conferencesService.deleteCalendarFeed(); setState("off"); } catch { setState("error"); }
  };
  return (
    <Popover.Root onOpenChange={(open) => { if (open) setState("idle"); }}>
      <Popover.Trigger asChild>
        <button type="button" aria-label="Add to my calendar" title="Add to my calendar" className={buttonClass}><CalendarPlus className="size-5" /></button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content align="end" sideOffset={6} collisionPadding={12} className="z-50 w-[min(92vw,20rem)] space-y-3 rounded-2xl border border-border bg-popover p-4 shadow-xl">
          <div>
            <p className="text-[15px] font-semibold text-foreground">Add to my calendar</p>
            <p className="mt-1 text-sm text-muted-foreground">See your meetings in Google, Outlook or Apple Calendar. Copy the link, then add it there as a calendar &quot;from URL&quot;.</p>
          </div>
          <button type="button" disabled={state === "busy"} onClick={() => void copy()} className="flex h-11 w-full items-center justify-center gap-2 rounded-xl bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60">
            {state === "copied" ? <><Check className="size-4" />Link copied</> : state === "busy" ? "Please wait…" : "Copy calendar link"}
          </button>
          {state === "copied" && <p role="status" className="text-xs text-muted-foreground">Keep this link private. Copying a new link turns the old one off.</p>}
          {state === "off" && <p role="status" className="text-xs text-muted-foreground">Your calendar link is off. Calendars using it stop updating.</p>}
          {state === "error" && <p role="alert" className="text-xs text-destructive">Something went wrong. Please try again.</p>}
          <button type="button" disabled={state === "busy"} onClick={() => void turnOff()} className="h-10 w-full rounded-xl text-sm font-medium text-destructive hover:bg-destructive/10 disabled:opacity-60">Turn off my calendar link</button>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
