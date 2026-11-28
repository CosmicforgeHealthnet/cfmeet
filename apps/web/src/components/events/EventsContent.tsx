"use client";

/**
 * Events (ADR 0071, owned by Messaging): Upcoming / Today / Past / Mine / Invitations /
 * Accepted, as a list grouped by day ("Load more" through the server's cursor) or a month
 * calendar (one `from` / `to` window per visible month). Search, type and chat filters are
 * server-side query parameters; nothing is filtered or invented on this device.
 */
import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { CalendarDays, CalendarPlus, ChevronLeft, ChevronRight, Download, List, Plus, Search, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { NiceSelect } from "@/components/ui/nice-select";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { viewerTimeZone } from "@/lib/meeting-time";
import { useEventList, useEventRange } from "@/hooks/v2/useEvents";
import { eventErrorMessage, eventsService, saveCalendarFile } from "@/services/v2/events.service";
import {
  EVENT_TYPES,
  EVENT_VIEWS,
  addMonths,
  eventHref,
  groupByDay,
  isEventId,
  localDate,
  monthGrid,
  monthRange,
  occurrencesByDate,
  type EventListView,
  type EventPageItem,
  type EventType,
} from "@/lib/events/core";
import EventForm from "./EventForm";
import { EventCover, TypeBadge, WhenLines, dayHeading, useMessagingSnapshot } from "./event-ui";

const RESPONSE_LABEL = { GOING: "Going", MAYBE: "Maybe", NOT_GOING: "Not going" } as const;
const WEEK_HEAD = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

export default function EventsContent() {
  const router = useRouter();
  const params = useSearchParams();
  const zone = useMemo(() => viewerTimeZone(), []);
  const requestedView = params.get("view")?.toUpperCase() as EventListView | undefined;
  const [view, setView] = useState<EventListView>(EVENT_VIEWS.some((v) => v.value === requestedView) ? requestedView! : "UPCOMING");
  const [mode, setMode] = useState<"list" | "month">(params.get("mode") === "month" ? "month" : "list");
  const [search, setSearch] = useState("");
  const q = useDebounced(search.trim(), 300);
  const [type, setType] = useState<EventType | "">("");
  const requestedChat = params.get("conversation");
  const [conversationId, setConversationId] = useState<string>(isEventId(requestedChat) ? requestedChat : "");
  const today = localDate(new Date().toISOString(), zone);
  const [month, setMonth] = useState(today.slice(0, 7));
  const [creating, setCreating] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const { snapshot } = useMessagingSnapshot();
  const chats = snapshot?.conversations ?? [];
  const chatTitle = (id: string | null | undefined) => (id ? chats.find((c) => c.id === id)?.title ?? null : null);

  // Keep the chat filter in the address (the chat's "Events" entry links here).
  const setChat = (id: string) => {
    setConversationId(id);
    const next = new URLSearchParams(params.toString());
    if (id) next.set("conversation", id); else next.delete("conversation");
    router.replace(`/dashboard/events${next.toString() ? `?${next}` : ""}`, { scroll: false });
  };

  const common = { conversationId: conversationId || null, q: q || undefined, type: type || null };
  const list = useEventList({ view, tz: zone, ...common }, mode === "list");
  const range = useMemo(() => monthRange(month, zone), [month, zone]);
  // A month shows its whole window; TODAY / PAST are time windows themselves, so they read as UPCOMING there.
  const monthView: EventListView = view === "TODAY" || view === "PAST" ? "UPCOMING" : view;
  const calendar = useEventRange({ view: monthView, from: range.from, to: range.to, ...common }, mode === "month");

  const exportAll = async () => {
    setExporting(true);
    setNotice(null);
    try {
      saveCalendarFile(await eventsService.exportCalendar(), "cf-meet-events");
      setNotice("Your events calendar file was downloaded.");
    } catch (e) {
      setNotice(eventErrorMessage(e));
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className="mx-auto w-full max-w-6xl px-4 pb-24 pt-5 sm:px-6 md:pb-10 lg:px-8">
      <header className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="text-xl font-bold tracking-tight sm:text-2xl">Events</h1>
          <p className="text-sm text-muted-foreground">Plans with your contacts and chats.</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => void exportAll()} disabled={exporting} title="Download your events as a calendar file">
          {exporting ? <Spinner size="sm" /> : <Download className="size-4" />}<span className="hidden sm:inline">Add to calendar</span>
        </Button>
        <Button size="sm" onClick={() => setCreating(true)} data-testid="create-event"><Plus className="size-4" />Create event</Button>
      </header>
      <p aria-live="polite" className="sr-only">{notice}</p>
      {notice ? <p className="mt-2 text-sm text-muted-foreground">{notice}</p> : null}

      {/* Views */}
      <div role="tablist" aria-label="Events" className="no-scrollbar -mx-4 mt-5 flex gap-1 overflow-x-auto border-b border-border px-4 sm:mx-0 sm:px-0">
        {EVENT_VIEWS.map((v) => (
          <button key={v.value} type="button" role="tab" aria-selected={view === v.value} onClick={() => setView(v.value)}
            className={cn("relative h-10 shrink-0 px-3 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", view === v.value ? "text-primary" : "text-muted-foreground hover:text-foreground")}>
            {v.label}
            {view === v.value ? <span className="absolute inset-x-2 -bottom-px h-0.5 rounded-full bg-primary" /> : null}
          </button>
        ))}
      </div>

      {/* Search and filters */}
      <div className="mt-4 flex flex-wrap items-end gap-2">
        <label className="relative min-w-[200px] flex-1">
          <span className="sr-only">Search events</span>
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
          <input type="search" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="Search title, description, place" maxLength={100}
            className="h-10 w-full rounded-xl border border-transparent bg-muted/70 pl-9 pr-3 text-sm focus:border-primary focus:bg-background focus:outline-none focus:ring-2 focus:ring-primary/20" />
        </label>
        <NiceSelect value={type} placeholder="All types" className="w-40" onChange={(v) => setType(v === "ALL" ? "" : (v as EventType))}
          options={[{ value: "ALL", label: "All types" }, ...EVENT_TYPES]} />
        {chats.length ? (
          <NiceSelect value={conversationId} placeholder="All chats" className="w-48" onChange={(v) => setChat(v === "ALL" ? "" : v)}
            options={[{ value: "ALL", label: "All chats" }, ...chats.filter((c) => c.title).map((c) => ({ value: c.id, label: c.title }))]} />
        ) : null}
        <div role="group" aria-label="Layout" className="flex rounded-xl bg-muted/70 p-1">
          <button type="button" aria-pressed={mode === "list"} onClick={() => setMode("list")} aria-label="List" title="List" className={cn("flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-sm", mode === "list" ? "bg-background font-medium shadow-sm" : "text-muted-foreground")}><List className="size-4" /><span className="hidden sm:inline">List</span></button>
          <button type="button" aria-pressed={mode === "month"} onClick={() => setMode("month")} aria-label="Month" title="Month" className={cn("flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-sm", mode === "month" ? "bg-background font-medium shadow-sm" : "text-muted-foreground")}><CalendarDays className="size-4" /><span className="hidden sm:inline">Month</span></button>
        </div>
      </div>
      {conversationId ? (
        <p className="mt-3 inline-flex items-center gap-2 rounded-full bg-primary/10 px-3 py-1 text-xs font-medium text-primary">
          Events in {chatTitle(conversationId) ?? "this chat"}
          <button type="button" onClick={() => setChat("")} aria-label="Show events from every chat" className="rounded-full p-0.5 hover:bg-primary/20"><X className="size-3" /></button>
        </p>
      ) : null}

      <div className="mt-5" role="tabpanel" aria-label={EVENT_VIEWS.find((v) => v.value === view)?.label}>
        {mode === "list" ? (
          list.isPending ? <Loading /> : list.isError ? <Failure message={eventErrorMessage(list.error)} retry={() => void list.refetch()} /> : list.items.length === 0 ? (
            <Empty view={view} filtered={!!(q || type || conversationId)} onCreate={() => setCreating(true)} />
          ) : (
            <>
              <div className="space-y-6">
                {groupByDay(list.items, zone).map((group) => (
                  <section key={group.date} aria-label={dayHeading(group.date, today)}>
                    <h2 className="mb-2 text-sm font-semibold text-muted-foreground">{dayHeading(group.date, today)}</h2>
                    <ul className="space-y-2">{group.items.map((item) => <li key={`${item.event.eventId}@${item.startsAt}`}><EventRow item={item} zone={zone} chat={chatTitle(item.event.conversationId)} /></li>)}</ul>
                  </section>
                ))}
              </div>
              {list.hasNextPage ? (
                <div className="mt-6 flex justify-center">
                  <Button variant="outline" size="sm" onClick={() => void list.fetchNextPage()} disabled={list.isFetchingNextPage}>{list.isFetchingNextPage ? <Spinner size="sm" /> : null}Load more</Button>
                </div>
              ) : null}
              {list.isFetchNextPageError ? <p role="alert" className="mt-2 text-center text-sm text-destructive">{eventErrorMessage(list.error)}</p> : null}
            </>
          )
        ) : (
          <MonthView month={month} setMonth={setMonth} today={today} zone={zone} loading={calendar.isPending} fetching={calendar.isFetching}
            error={calendar.isError ? eventErrorMessage(calendar.error) : null} retry={() => void calendar.refetch()} items={calendar.data ?? []} chatTitle={chatTitle} />
        )}
      </div>

      <EventForm open={creating} onClose={() => setCreating(false)} conversationId={conversationId || null}
        onSaved={(event) => { setCreating(false); router.push(eventHref(event.eventId)); }} />
    </div>
  );
}

function Loading() {
  return <div className="flex justify-center py-16" role="status" aria-label="Loading events"><Spinner /></div>;
}

function Failure({ message, retry }: { message: string; retry: () => void }) {
  return (
    <div role="alert" className="rounded-2xl border border-border p-6 text-center">
      <p className="text-sm text-destructive">{message}</p>
      <Button variant="outline" size="sm" className="mt-3" onClick={retry}>Try again</Button>
    </div>
  );
}

const EMPTY: Record<EventListView, string> = {
  UPCOMING: "Nothing planned yet. Create an event and invite your contacts or a chat.",
  TODAY: "Nothing on today.",
  PAST: "Past events show up here.",
  MINE: "Events you organize or co-organize show up here.",
  INVITATIONS: "No invitations waiting for an answer.",
  ACCEPTED: "Events you said you're going to show up here.",
};

function Empty({ view, filtered, onCreate }: { view: EventListView; filtered: boolean; onCreate: () => void }) {
  return (
    <div className="flex flex-col items-center rounded-3xl border border-dashed border-border px-6 py-14 text-center">
      <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><CalendarPlus className="size-7" aria-hidden /></span>
      <p className="mt-4 font-semibold">{filtered ? "No events match" : "No events"}</p>
      <p className="mt-1 max-w-sm text-sm text-muted-foreground">{filtered ? "Try another search or filter." : EMPTY[view]}</p>
      {!filtered && (view === "UPCOMING" || view === "MINE") ? <Button size="sm" className="mt-4" onClick={onCreate}><Plus className="size-4" />Create event</Button> : null}
    </div>
  );
}

function EventRow({ item, zone, chat }: { item: EventPageItem; zone: string; chat: string | null }) {
  const e = item.event;
  const cancelled = e.state === "CANCELLED";
  return (
    <Link href={eventHref(e.eventId)} className="flex gap-3 rounded-2xl border border-border bg-card p-3 transition-colors hover:border-primary/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
      <EventCover mediaId={e.coverMediaId} className="size-16 shrink-0 rounded-xl sm:size-20" />
      <span className="min-w-0 flex-1">
        <span className={cn("block truncate font-semibold", cancelled && "text-muted-foreground line-through")}>{e.title}</span>
        <WhenLines startsAt={item.startsAt} endsAt={item.endsAt} event={e} viewerZone={zone} className="mt-0.5 block text-sm" />
        <span className="mt-1.5 flex flex-wrap items-center gap-1.5">
          <TypeBadge type={e.type} />
          {cancelled ? <span className="rounded-full bg-destructive/10 px-2 py-0.5 text-xs font-medium text-destructive">Cancelled</span> : null}
          {e.myResponse ? <span className="rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">{RESPONSE_LABEL[e.myResponse]}</span> : null}
          {e.myRole === "ORGANIZER" || e.myRole === "CO_ORGANIZER" ? <span className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">{e.myRole === "ORGANIZER" ? "Organizer" : "Co-organizer"}</span> : null}
          {chat ? <span className="truncate text-xs text-muted-foreground">in {chat}</span> : null}
          {e.location ? <span className="truncate text-xs text-muted-foreground">· {e.location}</span> : null}
        </span>
      </span>
    </Link>
  );
}

function MonthView({ month, setMonth, today, zone, items, loading, fetching, error, retry, chatTitle }: {
  month: string; setMonth: (m: string) => void; today: string; zone: string; items: EventPageItem[]; loading: boolean; fetching: boolean;
  error: string | null; retry: () => void; chatTitle: (id: string | null) => string | null;
}) {
  const weeks = useMemo(() => monthGrid(month), [month]);
  const byDate = useMemo(() => occurrencesByDate(items, zone), [items, zone]);
  const [selected, setSelected] = useState<string | null>(null);
  const label = new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric", timeZone: "UTC" }).format(new Date(`${month}-15T12:00:00Z`));
  const go = (n: number) => { setMonth(addMonths(`${month}-01`, n).slice(0, 7)); setSelected(null); };
  const dayItems = selected ? byDate.get(selected) ?? [] : [];
  return (
    <div>
      <div className="mb-3 flex items-center gap-2">
        <h2 className="flex-1 text-lg font-semibold" aria-live="polite">{label}{fetching && !loading ? <Spinner size="sm" className="ml-2 inline-block align-middle" /> : null}</h2>
        <Button variant="outline" size="sm" onClick={() => { setMonth(today.slice(0, 7)); setSelected(today); }}>Today</Button>
        <button type="button" onClick={() => go(-1)} aria-label="Previous month" className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ChevronLeft className="size-5" /></button>
        <button type="button" onClick={() => go(1)} aria-label="Next month" className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ChevronRight className="size-5" /></button>
      </div>
      {error ? <Failure message={error} retry={retry} /> : loading ? <Loading /> : (
        <>
          <div role="grid" aria-label={label} className="overflow-hidden rounded-2xl border border-border">
            <div role="row" className="grid grid-cols-7 border-b border-border bg-muted/40">
              {WEEK_HEAD.map((d) => <div key={d} role="columnheader" className="py-2 text-center text-xs font-semibold text-muted-foreground">{d}</div>)}
            </div>
            {weeks.map((week) => (
              <div role="row" key={week[0].date} className="grid grid-cols-7 border-b border-border last:border-b-0">
                {week.map((cell) => {
                  const list = byDate.get(cell.date) ?? [];
                  const isToday = cell.date === today;
                  const day = Number(cell.date.slice(8));
                  return (
                    <div role="gridcell" key={cell.date} aria-selected={selected === cell.date} className={cn("min-h-16 border-r border-border p-1 last:border-r-0 sm:min-h-28", !cell.inMonth && "bg-muted/30")}>
                      <button type="button" onClick={() => setSelected(cell.date)} aria-label={`${dayHeading(cell.date, today)}, ${list.length} ${list.length === 1 ? "event" : "events"}`}
                        className={cn("flex size-7 items-center justify-center rounded-full text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", isToday ? "bg-primary text-primary-foreground" : selected === cell.date ? "bg-primary/15 text-primary" : cell.inMonth ? "text-foreground hover:bg-muted" : "text-muted-foreground hover:bg-muted")}>
                        {day}
                      </button>
                      {/* Phones: dots; wider screens: titles. */}
                      {list.length ? <span className="ml-1 flex gap-0.5 sm:hidden" aria-hidden>{list.slice(0, 3).map((i) => <span key={`${i.event.eventId}@${i.startsAt}`} className={cn("size-1.5 rounded-full", i.event.state === "CANCELLED" ? "bg-muted-foreground/40" : "bg-primary")} />)}</span> : null}
                      <ul className="mt-0.5 hidden space-y-0.5 sm:block">
                        {list.slice(0, 3).map((i) => (
                          <li key={`${i.event.eventId}@${i.startsAt}`}>
                            <Link href={eventHref(i.event.eventId)} className={cn("block truncate rounded-md px-1.5 py-0.5 text-[11px] font-medium", i.event.state === "CANCELLED" ? "text-muted-foreground line-through" : "bg-primary/10 text-primary hover:bg-primary/20")}>
                              {i.event.allDay ? "" : `${new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: zone }).format(new Date(i.startsAt))} `}{i.event.title}
                            </Link>
                          </li>
                        ))}
                        {list.length > 3 ? <li><button type="button" onClick={() => setSelected(cell.date)} className="px-1.5 text-[11px] font-medium text-muted-foreground hover:text-foreground">+{list.length - 3} more</button></li> : null}
                      </ul>
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
          {selected ? (
            <section className="mt-4" aria-label={dayHeading(selected, today)}>
              <h3 className="mb-2 text-sm font-semibold text-muted-foreground">{dayHeading(selected, today)}</h3>
              {dayItems.length ? <ul className="space-y-2">{dayItems.map((item) => <li key={`${item.event.eventId}@${item.startsAt}`}><EventRow item={item} zone={zone} chat={chatTitle(item.event.conversationId)} /></li>)}</ul>
                : <p className="text-sm text-muted-foreground">Nothing on this day.</p>}
            </section>
          ) : null}
        </>
      )}
    </div>
  );
}
