/**
 * Events (ADR 0071: Messaging owns them; the web reaches them through the Meeting relay at
 * `/api/v2/messaging/relay/v1/messaging/events/...`). The pure rules of the screens: wire
 * types, form state → request bodies (local date / time in an IANA zone → UTC instants with
 * the backend's DST rules), recurrence text, month grids, grouping by day, list queries and
 * cursor paging. No React, no network: tested in tests/events.test.mjs.
 */
import { utcToZonedLocal, zonedLocalToUtc } from "../meeting-time";

// ---------------------------------------------------------------------------- wire types

export type EventScope = "PERSONAL" | "CONVERSATION" | "ORGANIZATION" | "COMMUNITY";
export type EventType = "ONLINE" | "PHYSICAL" | "CALL";
export type EventListView = "UPCOMING" | "TODAY" | "PAST" | "MINE" | "INVITATIONS" | "ACCEPTED";
export type RsvpResponse = "GOING" | "MAYBE" | "NOT_GOING";
export type EventRole = "ORGANIZER" | "CO_ORGANIZER" | "INVITEE" | "MEMBER";
export type Weekday = "MO" | "TU" | "WE" | "TH" | "FR" | "SA" | "SU";
export type Frequency = "DAILY" | "WEEKLY" | "MONTHLY";

export interface Recurrence {
  frequency: Frequency;
  interval?: number | null;
  count?: number | null;
  /** A local date `YYYY-MM-DD` (inclusive) in the event's zone. */
  until?: string | null;
  byDay?: Weekday[] | null;
}

export interface EventCounts { going: number; maybe: number; notGoing: number; awaiting: number }
export interface Occurrence { startsAt: string; endsAt: string }

export interface CalendarEvent {
  eventId: string;
  scope: EventScope;
  conversationId: string | null;
  /** A COMMUNITY event's community (ADR 0071 / 0073). */
  communityId?: string | null;
  title: string;
  description: string | null;
  location: string | null;
  type: EventType;
  meetingId: string | null;
  meetingUrl: string | null;
  coverMediaId: string | null;
  startsAt: string;
  endsAt: string;
  timeZone: string;
  allDay: boolean;
  recurrence: Recurrence | null;
  rrule: string | null;
  seriesEndsAt: string | null;
  state: "ACTIVE" | "CANCELLED";
  cancelledAt: string | null;
  cancelReason: string | null;
  organizerPrincipalId: string;
  coOrganizerPrincipalIds: string[];
  myRole: EventRole | null;
  myResponse: RsvpResponse | null;
  myReminders: number[];
  counts: EventCounts;
  canEdit: boolean;
  canDelete: boolean;
  version: number;
  createdAt: string;
  updatedAt: string;
  nextOccurrence: Occurrence | null;
  /** `getEvent` only (at most 10). */
  upcomingOccurrences?: Occurrence[];
}

export interface EventPageItem { startsAt: string; endsAt: string; event: CalendarEvent }
export interface EventPage { items: EventPageItem[]; nextCursor: string | null }

export interface EventCreateRequest {
  scope: EventScope;
  conversationId?: string;
  communityId?: string;
  title: string;
  description?: string;
  location?: string;
  type: EventType;
  meetingId?: string;
  meetingUrl?: string;
  coverMediaId?: string;
  startsAt: string;
  endsAt: string;
  timeZone: string;
  allDay: boolean;
  recurrence?: Recurrence;
  coOrganizerPrincipalIds?: string[];
  inviteePrincipalIds?: string[];
  remindersMinutesBefore?: number[];
}

/** Every editable field (`null` clears an optional one) plus the `version` read. */
export type EventUpdateRequest = Partial<{
  title: string;
  description: string | null;
  location: string | null;
  type: EventType;
  meetingId: string | null;
  meetingUrl: string | null;
  coverMediaId: string | null;
  startsAt: string;
  endsAt: string;
  timeZone: string;
  allDay: boolean;
  recurrence: Recurrence | null;
  coOrganizerPrincipalIds: string[];
}> & { version: number };

// ---------------------------------------------------------------------------- limits

export const LIMITS = {
  title: 200,
  description: 5000,
  location: 500,
  coOrganizers: 10,
  invitees: 500,
  reminders: 5,
  reminderMaxMinutes: 40320,
  maxDurationDays: 31,
  interval: 99,
  count: 730,
  untilYears: 5,
  calendarDays: 92,
  pageSize: 30,
} as const;

export const EVENT_VIEWS: { value: EventListView; label: string }[] = [
  { value: "UPCOMING", label: "Upcoming" },
  { value: "TODAY", label: "Today" },
  { value: "PAST", label: "Past" },
  { value: "MINE", label: "Mine" },
  { value: "INVITATIONS", label: "Invitations" },
  { value: "ACCEPTED", label: "Accepted" },
];

export const EVENT_TYPES: { value: EventType; label: string }[] = [
  { value: "ONLINE", label: "Online" },
  { value: "PHYSICAL", label: "In person" },
  { value: "CALL", label: "Call" },
];

export const REMINDER_CHOICES = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080] as const;

export const WEEKDAYS: Weekday[] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
export const WEEKDAY_SHORT: Record<Weekday, string> = { MO: "Mon", TU: "Tue", WE: "Wed", TH: "Thu", FR: "Fri", SA: "Sat", SU: "Sun" };
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// ---------------------------------------------------------------------------- dates

const DATE = /^(\d{4})-(\d{2})-(\d{2})$/;
const TIME = /^([01]\d|2[0-3]):([0-5]\d)$/;
const pad = (n: number, size = 2) => String(n).padStart(size, "0");

function parseDate(date: string): { y: number; m: number; d: number } {
  const match = DATE.exec(date);
  if (!match) throw new Error("Choose a date.");
  return { y: +match[1], m: +match[2], d: +match[3] };
}

/** Calendar arithmetic on `YYYY-MM-DD` (zone-free). */
export function addDays(date: string, days: number): string {
  const { y, m, d } = parseDate(date);
  const t = new Date(Date.UTC(y, m - 1, d + days));
  return `${pad(t.getUTCFullYear(), 4)}-${pad(t.getUTCMonth() + 1)}-${pad(t.getUTCDate())}`;
}

/** Whole days from `a` to `b`. */
export function daysBetween(a: string, b: string): number {
  const p = parseDate(a), q = parseDate(b);
  return Math.round((Date.UTC(q.y, q.m - 1, q.d) - Date.UTC(p.y, p.m - 1, p.d)) / 86_400_000);
}

/** The RRULE weekday of a local date. */
export function weekdayOf(date: string): Weekday {
  const { y, m, d } = parseDate(date);
  return WEEKDAYS[(new Date(Date.UTC(y, m - 1, d)).getUTCDay() + 6) % 7];
}

/** The local date of an instant in a zone. */
export function localDate(iso: string, timeZone: string): string {
  return utcToZonedLocal(iso, timeZone).slice(0, 10);
}

/** "28 Nov 2026" (fixed English, so the tests are stable). */
export function shortDate(date: string): string {
  const { y, m, d } = parseDate(date);
  return `${d} ${MONTHS[m - 1]} ${y}`;
}

// ---------------------------------------------------------------------------- form state

export type RecurrenceChoice = "NONE" | Frequency;
export type RecurrenceEnd = "NEVER" | "COUNT" | "UNTIL";

export interface EventFormState {
  title: string;
  description: string;
  location: string;
  type: EventType;
  meetingId: string;
  meetingUrl: string;
  coverMediaId: string | null;
  startDate: string;
  startTime: string;
  endDate: string;
  endTime: string;
  allDay: boolean;
  timeZone: string;
  frequency: RecurrenceChoice;
  interval: number;
  byDay: Weekday[];
  ends: RecurrenceEnd;
  count: number;
  until: string;
  scope: "PERSONAL" | "CONVERSATION" | "COMMUNITY";
  conversationId: string;
  communityId: string;
  coOrganizerPrincipalIds: string[];
  inviteePrincipalIds: string[];
  reminders: number[];
}

/** A new event: the next whole hour for one hour, in the given zone (in a chat or a community when given). */
export function emptyForm(timeZone: string, now = Date.now(), conversationId?: string | null, communityId?: string | null): EventFormState {
  const next = Math.ceil((now + 60_000) / 3_600_000) * 3_600_000;
  const start = utcToZonedLocal(new Date(next).toISOString(), timeZone);
  const end = utcToZonedLocal(new Date(next + 3_600_000).toISOString(), timeZone);
  return {
    title: "", description: "", location: "", type: "PHYSICAL", meetingId: "", meetingUrl: "", coverMediaId: null,
    startDate: start.slice(0, 10), startTime: start.slice(11, 16), endDate: end.slice(0, 10), endTime: end.slice(11, 16),
    allDay: false, timeZone, frequency: "NONE", interval: 1, byDay: [weekdayOf(start.slice(0, 10))], ends: "NEVER", count: 10, until: "",
    scope: communityId ? "COMMUNITY" : conversationId ? "CONVERSATION" : "PERSONAL",
    conversationId: communityId ? "" : conversationId ?? "", communityId: communityId ?? "",
    coOrganizerPrincipalIds: [], inviteePrincipalIds: [], reminders: [30],
  };
}

/** The form of an existing event (its own zone; an all-day end shows the last day). */
export function formFromEvent(event: CalendarEvent): EventFormState {
  const zone = event.timeZone || "UTC";
  const start = utcToZonedLocal(event.startsAt, zone);
  const end = utcToZonedLocal(event.endsAt, zone);
  const r = event.recurrence;
  return {
    title: event.title, description: event.description ?? "", location: event.location ?? "", type: event.type,
    meetingId: event.meetingId ?? "", meetingUrl: event.meetingUrl ?? "", coverMediaId: event.coverMediaId,
    startDate: start.slice(0, 10), startTime: start.slice(11, 16),
    endDate: event.allDay ? addDays(end.slice(0, 10), -1) : end.slice(0, 10), endTime: end.slice(11, 16),
    allDay: event.allDay, timeZone: zone,
    frequency: r ? r.frequency : "NONE", interval: r?.interval ?? 1,
    byDay: r?.byDay?.length ? [...r.byDay] : [weekdayOf(start.slice(0, 10))],
    ends: r?.count ? "COUNT" : r?.until ? "UNTIL" : "NEVER", count: r?.count ?? 10, until: r?.until ?? "",
    scope: event.scope === "CONVERSATION" || event.scope === "COMMUNITY" ? event.scope : "PERSONAL",
    conversationId: event.conversationId ?? "", communityId: event.communityId ?? "",
    coOrganizerPrincipalIds: [...(event.coOrganizerPrincipalIds ?? [])], inviteePrincipalIds: [], reminders: [...(event.myReminders ?? [])],
  };
}

/**
 * Start and end instants. Timed: local date + time in `timeZone`. All day: the local
 * midnight of the first day and the midnight after the last day.
 */
export function eventTimes(state: Pick<EventFormState, "startDate" | "startTime" | "endDate" | "endTime" | "allDay" | "timeZone">): { startsAt: string; endsAt: string } {
  parseDate(state.startDate);
  parseDate(state.endDate);
  if (state.allDay) {
    return {
      startsAt: zonedLocalToUtc(`${state.startDate}T00:00`, state.timeZone),
      endsAt: zonedLocalToUtc(`${addDays(state.endDate, 1)}T00:00`, state.timeZone),
    };
  }
  if (!TIME.test(state.startTime) || !TIME.test(state.endTime)) throw new Error("Choose a start and end time.");
  return {
    startsAt: zonedLocalToUtc(`${state.startDate}T${state.startTime}`, state.timeZone),
    endsAt: zonedLocalToUtc(`${state.endDate}T${state.endTime}`, state.timeZone),
  };
}

/** The recurrence of the form (null: a single event). */
export function recurrenceOf(state: Pick<EventFormState, "frequency" | "interval" | "byDay" | "ends" | "count" | "until" | "startDate">): Recurrence | null {
  if (state.frequency === "NONE") return null;
  const out: Recurrence = { frequency: state.frequency, interval: Math.max(1, Math.round(state.interval || 1)) };
  if (state.frequency === "WEEKLY") out.byDay = WEEKDAYS.filter((d) => state.byDay.includes(d));
  if (state.ends === "COUNT") out.count = Math.round(state.count);
  if (state.ends === "UNTIL") out.until = state.until;
  return out;
}

/** What is wrong with the form, in words (null: it can be sent). The server stays the authority. */
export function validateForm(state: EventFormState, now = Date.now()): string | null {
  const title = state.title.trim();
  if (!title) return "Give the event a title.";
  if (title.length > LIMITS.title) return `The title can be at most ${LIMITS.title} characters.`;
  if (state.description.length > LIMITS.description) return `The description can be at most ${LIMITS.description} characters.`;
  if (state.location.length > LIMITS.location) return `The location can be at most ${LIMITS.location} characters.`;
  if (state.type === "ONLINE" && !state.meetingId && !state.meetingUrl) return "Pick a CF Meet meeting for an online event, or create one.";
  if (state.meetingUrl && !/^https:\/\//i.test(state.meetingUrl)) return "The meeting link must start with https://.";
  if (state.scope === "CONVERSATION" && !state.conversationId) return "Choose the chat this event belongs to.";
  if (state.scope === "COMMUNITY" && !state.communityId) return "Choose the community this event belongs to.";
  let times: { startsAt: string; endsAt: string };
  try {
    times = eventTimes(state);
  } catch (e) {
    return (e as Error).message || "Choose a start and end.";
  }
  const start = Date.parse(times.startsAt), end = Date.parse(times.endsAt);
  if (end <= start) return "The end must be after the start.";
  if (end - start > LIMITS.maxDurationDays * 86_400_000) return `An event can last at most ${LIMITS.maxDurationDays} days.`;
  if (state.frequency === "NONE" && end <= now) return "This time has already passed. Choose a time in the future.";
  if (state.frequency !== "NONE") {
    if (!(state.interval >= 1 && state.interval <= LIMITS.interval)) return `Repeat every 1 to ${LIMITS.interval}.`;
    if (state.frequency === "WEEKLY") {
      if (!state.byDay.length) return "Choose at least one day of the week.";
      if (!state.byDay.includes(weekdayOf(state.startDate))) return `The first day (${WEEKDAY_SHORT[weekdayOf(state.startDate)]}) must be one of the chosen days.`;
    }
    if (state.ends === "COUNT" && !(state.count >= 1 && state.count <= LIMITS.count)) return `Repeat 1 to ${LIMITS.count} times.`;
    if (state.ends === "UNTIL") {
      if (!DATE.test(state.until)) return "Choose the last date of the series.";
      if (state.until < state.startDate) return "The series can't end before it starts.";
      if (daysBetween(state.startDate, state.until) > LIMITS.untilYears * 366) return `A series can run for at most ${LIMITS.untilYears} years.`;
    }
  }
  if (state.coOrganizerPrincipalIds.length > LIMITS.coOrganizers) return `At most ${LIMITS.coOrganizers} co-organizers.`;
  if (state.inviteePrincipalIds.length > LIMITS.invitees) return `At most ${LIMITS.invitees} people can be invited.`;
  if (state.reminders.length > LIMITS.reminders) return `At most ${LIMITS.reminders} reminders.`;
  return null;
}

const trimmed = (s: string) => s.trim();

/** `POST /v1/messaging/events` body. */
export function buildCreateBody(state: EventFormState): EventCreateRequest {
  const { startsAt, endsAt } = eventTimes(state);
  const recurrence = recurrenceOf(state);
  const body: EventCreateRequest = {
    scope: state.scope,
    title: trimmed(state.title),
    type: state.type,
    startsAt,
    endsAt,
    timeZone: state.timeZone,
    allDay: state.allDay,
  };
  if (state.scope === "CONVERSATION") body.conversationId = state.conversationId;
  if (state.scope === "COMMUNITY") body.communityId = state.communityId;
  if (trimmed(state.description)) body.description = trimmed(state.description);
  if (trimmed(state.location)) body.location = trimmed(state.location);
  if (state.type === "ONLINE") {
    if (state.meetingId) body.meetingId = state.meetingId;
    if (state.meetingUrl) body.meetingUrl = state.meetingUrl;
  }
  if (state.coverMediaId) body.coverMediaId = state.coverMediaId;
  if (recurrence) body.recurrence = recurrence;
  const co = [...new Set(state.coOrganizerPrincipalIds)];
  if (co.length) body.coOrganizerPrincipalIds = co;
  const invitees = [...new Set(state.inviteePrincipalIds)].filter((p) => !co.includes(p));
  if (invitees.length) body.inviteePrincipalIds = invitees;
  body.remindersMinutesBefore = [...new Set(state.reminders)].sort((a, b) => a - b);
  return body;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

function normalRecurrence(r: Recurrence | null | undefined): Recurrence | null {
  if (!r) return null;
  const out: Recurrence = { frequency: r.frequency, interval: r.interval ?? 1 };
  if (r.frequency === "WEEKLY" && r.byDay?.length) out.byDay = WEEKDAYS.filter((d) => r.byDay!.includes(d));
  if (r.count) out.count = r.count;
  if (r.until) out.until = r.until;
  return out;
}

/** `PATCH /v1/messaging/events/{id}` body: only what changed, `null` clears, plus `version`. */
export function buildPatchBody(state: EventFormState, original: CalendarEvent): EventUpdateRequest {
  const { startsAt, endsAt } = eventTimes(state);
  const online = state.type === "ONLINE";
  const next = {
    title: trimmed(state.title),
    description: trimmed(state.description) || null,
    location: trimmed(state.location) || null,
    type: state.type,
    meetingId: online ? state.meetingId || null : null,
    meetingUrl: online ? state.meetingUrl || null : null,
    coverMediaId: state.coverMediaId || null,
    startsAt,
    endsAt,
    timeZone: state.timeZone,
    allDay: state.allDay,
    recurrence: normalRecurrence(recurrenceOf(state)),
    coOrganizerPrincipalIds: [...new Set(state.coOrganizerPrincipalIds)],
  };
  const before: Record<keyof typeof next, unknown> = {
    title: original.title,
    description: original.description || null,
    location: original.location || null,
    type: original.type,
    meetingId: original.meetingId || null,
    meetingUrl: original.meetingUrl || null,
    coverMediaId: original.coverMediaId || null,
    startsAt: new Date(original.startsAt).toISOString(),
    endsAt: new Date(original.endsAt).toISOString(),
    timeZone: original.timeZone,
    allDay: original.allDay,
    recurrence: normalRecurrence(original.recurrence),
    coOrganizerPrincipalIds: original.coOrganizerPrincipalIds ?? [],
  };
  const patch: EventUpdateRequest = { version: original.version };
  for (const key of Object.keys(next) as (keyof typeof next)[]) {
    if (key === "coOrganizerPrincipalIds" ? !same([...next[key]].sort(), [...(before[key] as string[])].sort()) : !same(next[key], before[key])) {
      (patch as Record<string, unknown>)[key] = next[key];
    }
  }
  return patch;
}

// ---------------------------------------------------------------------------- text

/** "Every 2 weeks on Mon, Wed, 10 times" / "Every month on day 15, until 31 Dec 2026". */
export function recurrenceSummary(r: Recurrence | null | undefined, startDate?: string | null): string {
  if (!r) return "Does not repeat";
  const n = r.interval && r.interval > 1 ? r.interval : 1;
  const unit = r.frequency === "DAILY" ? "day" : r.frequency === "WEEKLY" ? "week" : "month";
  let text = n === 1 ? `Every ${unit}` : `Every ${n} ${unit}s`;
  if (r.frequency === "WEEKLY") {
    const days = WEEKDAYS.filter((d) => r.byDay?.includes(d));
    if (days.length === 7) text += " on every day";
    else if (days.length) text += ` on ${days.map((d) => WEEKDAY_SHORT[d]).join(", ")}`;
  }
  if (r.frequency === "MONTHLY" && startDate && DATE.test(startDate)) text += ` on day ${parseDate(startDate).d}`;
  if (r.count) text += r.count === 1 ? ", once" : `, ${r.count} times`;
  else if (r.until && DATE.test(r.until)) text += `, until ${shortDate(r.until)}`;
  return text;
}

/** "At start of event" / "10 minutes before" / "1 hour before" / "2 days before" / "1 week before". */
export function reminderLabel(minutes: number): string {
  if (minutes <= 0) return "At start of event";
  const plural = (v: number, u: string) => `${v} ${u}${v === 1 ? "" : "s"} before`;
  if (minutes % 10080 === 0) return plural(minutes / 10080, "week");
  if (minutes % 1440 === 0) return plural(minutes / 1440, "day");
  if (minutes % 60 === 0) return plural(minutes / 60, "hour");
  return plural(minutes, "minute");
}

// ---------------------------------------------------------------------------- lists

export interface EventFilters {
  view: EventListView;
  /** The browser's IANA zone (TODAY is sent as that zone's day, from / to). */
  tz?: string;
  from?: string;
  to?: string;
  conversationId?: string | null;
  q?: string;
  type?: EventType | null;
  limit?: number;
  cursor?: string | null;
}

/** The relay's order of `GET messaging/events` query keys. */
export const LIST_QUERY_KEYS = ["view", "from", "to", "conversationId", "q", "type", "limit", "cursor"] as const;

/** The caller's day in `timeZone` as a UTC window (`to` exclusive). */
export function todayRange(timeZone: string, now = Date.now()): { from: string; to: string } {
  const day = localDate(new Date(now).toISOString(), timeZone);
  return { from: zonedLocalToUtc(`${day}T00:00`, timeZone), to: zonedLocalToUtc(`${addDays(day, 1)}T00:00`, timeZone) };
}

/** `?view=…&…` for `GET v1/messaging/events` (empty values omitted; the relay takes at most 8 keys). */
export function listQuery(filters: EventFilters, now = Date.now()): string {
  const q = (filters.q ?? "").trim().slice(0, 100);
  // TODAY is the caller's own day: sent as its window, so the server needs no zone.
  const today = filters.view === "TODAY" && !filters.from && filters.tz ? todayRange(filters.tz, now) : null;
  const values: Record<(typeof LIST_QUERY_KEYS)[number], string | undefined> = {
    view: filters.view,
    from: filters.from || today?.from || undefined,
    to: filters.to || today?.to || undefined,
    conversationId: filters.conversationId || undefined,
    q: q || undefined,
    type: filters.type || undefined,
    limit: String(Math.min(100, Math.max(1, filters.limit ?? LIMITS.pageSize))),
    cursor: filters.cursor || undefined,
  };
  const params = new URLSearchParams();
  for (const key of LIST_QUERY_KEYS) if (values[key] !== undefined) params.set(key, values[key]!);
  return params.toString();
}

/**
 * The occurrences of one community's events (ADR 0071 / 0073). `GET messaging/events` takes
 * at most 8 query keys and has no community filter, so the community page filters the
 * caller's visible events (the server already limits them to communities they belong to).
 */
export function communityEventItems(items: readonly EventPageItem[], communityId: string): EventPageItem[] {
  return items.filter((i) => i.event.scope === "COMMUNITY" && i.event.communityId === communityId);
}

const itemKey = (i: Pick<EventPageItem, "startsAt"> & { event: Pick<CalendarEvent, "eventId"> }) => `${i.event.eventId}@${new Date(i.startsAt).toISOString()}`;

/** Every loaded page in order, without an occurrence twice. */
export function mergeEventPages(pages: readonly Pick<EventPage, "items">[] | undefined): EventPageItem[] {
  const seen = new Set<string>();
  const out: EventPageItem[] = [];
  for (const page of pages ?? []) {
    for (const item of page.items) {
      const k = itemKey(item);
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(item);
    }
  }
  return out;
}

/** The zone an occurrence's day is read in: an all-day event keeps its own dates. */
const dayZone = (item: EventPageItem, viewerZone: string) => (item.event.allDay ? item.event.timeZone || viewerZone : viewerZone);

/** Occurrences grouped by the local date they start on (list view), in order of first appearance. */
export function groupByDay(items: readonly EventPageItem[], viewerZone: string): { date: string; items: EventPageItem[] }[] {
  const groups = new Map<string, EventPageItem[]>();
  for (const item of items) {
    const date = localDate(item.startsAt, dayZone(item, viewerZone));
    const list = groups.get(date) ?? [];
    list.push(item);
    groups.set(date, list);
  }
  return [...groups].map(([date, list]) => ({ date, items: list }));
}

/** Each local date an occurrence covers (an all-day / multi-day event spans several cells). */
export function occurrenceDates(item: EventPageItem, viewerZone: string): string[] {
  const zone = dayZone(item, viewerZone);
  const first = localDate(item.startsAt, zone);
  // The end is exclusive: an event ending at midnight doesn't show on the next day.
  const endInstant = new Date(Math.max(Date.parse(item.startsAt), Date.parse(item.endsAt) - 1)).toISOString();
  const last = localDate(endInstant, zone);
  const out: string[] = [];
  for (let d = first; d <= last && out.length < 62; d = addDays(d, 1)) out.push(d);
  return out;
}

export interface MonthCell { date: string; inMonth: boolean }

/** The weeks of a month grid (`month`: `YYYY-MM`), Monday first by default. */
export function monthGrid(month: string, weekStartsOn: 0 | 1 = 1): MonthCell[][] {
  const first = `${month}-01`;
  parseDate(first);
  const offset = (new Date(`${first}T00:00:00Z`).getUTCDay() - weekStartsOn + 7) % 7;
  const start = addDays(first, -offset);
  const nextMonth = addMonths(first, 1);
  const weeks: MonthCell[][] = [];
  let d = start;
  do {
    const week: MonthCell[] = [];
    for (let i = 0; i < 7; i++, d = addDays(d, 1)) week.push({ date: d, inMonth: d.slice(0, 7) === month });
    weeks.push(week);
  } while (d < nextMonth);
  return weeks;
}

/** `YYYY-MM-DD` plus whole months (clamped to the month's last day). */
export function addMonths(date: string, months: number): string {
  const { y, m, d } = parseDate(date);
  const total = y * 12 + (m - 1) + months;
  const ny = Math.floor(total / 12), nm = total % 12;
  const last = new Date(Date.UTC(ny, nm + 1, 0)).getUTCDate();
  return `${pad(ny, 4)}-${pad(nm + 1)}-${pad(Math.min(d, last))}`;
}

/** The `from` / `to` (UTC, `to` exclusive) of a month grid as the viewer sees it (≤ 42 days). */
export function monthRange(month: string, viewerZone: string, weekStartsOn: 0 | 1 = 1): { from: string; to: string } {
  const weeks = monthGrid(month, weekStartsOn);
  const firstDay = weeks[0][0].date;
  const lastDay = weeks[weeks.length - 1][6].date;
  return { from: zonedLocalToUtc(`${firstDay}T00:00`, viewerZone), to: zonedLocalToUtc(`${addDays(lastDay, 1)}T00:00`, viewerZone) };
}

/** Occurrences per local date of a grid. */
export function occurrencesByDate(items: readonly EventPageItem[], viewerZone: string): Map<string, EventPageItem[]> {
  const out = new Map<string, EventPageItem[]>();
  for (const item of items) {
    for (const date of occurrenceDates(item, viewerZone)) {
      const list = out.get(date) ?? [];
      list.push(item);
      out.set(date, list);
    }
  }
  for (const list of out.values()) list.sort((a, b) => Number(b.event.allDay) - Number(a.event.allDay) || a.startsAt.localeCompare(b.startsAt));
  return out;
}

// ---------------------------------------------------------------------------- links

const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
export const EVENT_ROUTE = "/dashboard/events";
const UUID_RE = new RegExp(`^${UUID}$`, "i");

export const isEventId = (value: string | null | undefined): value is string => !!value && UUID_RE.test(value);

/** The event page. */
export function eventHref(eventId: string): string {
  if (!isEventId(eventId)) throw new Error("Invalid event id");
  return `${EVENT_ROUTE}/${eventId.toLowerCase()}`;
}

/** The shareable link of an event (`https://…/dashboard/events/<id>`). */
export function eventLink(origin: string, eventId: string): string {
  return `${origin.replace(/\/+$/, "")}${eventHref(eventId)}`;
}

/** The text a chat message carries for an event (its title and link). */
export function eventShareText(origin: string, event: Pick<CalendarEvent, "eventId" | "title">): string {
  return `Event: ${event.title.trim().slice(0, 200)}\n${eventLink(origin, event.eventId)}`;
}

/** Event ids linked in a message (only links to this app's own origin), at most 3. */
export function eventIdsInText(text: string | null | undefined, origin: string): string[] {
  if (!text) return [];
  const escaped = origin.replace(/\/+$/, "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`${escaped}${EVENT_ROUTE}/(${UUID})(?![0-9a-z-])`, "gi");
  const out: string[] = [];
  for (const m of text.matchAll(re)) {
    const id = m[1].toLowerCase();
    if (!out.includes(id)) out.push(id);
    if (out.length === 3) break;
  }
  return out;
}

/** Where "Join meeting" goes: the stored join URL (same-origin as an in-app path), else the meeting page. */
export function meetingTarget(event: Pick<CalendarEvent, "meetingId" | "meetingUrl">, origin: string): { href: string; external: boolean } | null {
  if (event.meetingUrl) {
    try {
      const url = new URL(event.meetingUrl);
      if (url.protocol !== "https:" && url.origin !== origin) return null;
      if (url.origin === origin) return { href: `${url.pathname}${url.search}`, external: false };
      return { href: url.toString(), external: true };
    } catch {
      return null;
    }
  }
  if (event.meetingId && /^[A-Za-z0-9_-]{1,64}$/.test(event.meetingId)) return { href: `/dashboard/meetings/${encodeURIComponent(event.meetingId)}`, external: false };
  return null;
}

/** A meeting's join URL to store on the event (`https` only, as the server requires). */
export function meetingJoinUrl(origin: string, meetingCode: string): string | null {
  if (!/^https:\/\//i.test(origin) || !/^[A-Za-z0-9-]{3,64}$/.test(meetingCode)) return null;
  return `${origin.replace(/\/+$/, "")}/${encodeURIComponent(meetingCode)}`;
}

/** The ICS file name to save (always `.ics`, no path characters). */
export function icsFilename(filename: string | null | undefined, fallback = "event"): string {
  const base = (filename || fallback).replace(/\.ics$/i, "").replace(/[^\w.-]+/g, "-").replace(/^[-.]+|[-.]+$/g, "").slice(0, 80) || fallback;
  return `${base}.ics`;
}
