/**
 * Calendar views over the one agenda query (Blueprint Section 10 / 17):
 * `GET /api/v2/me/conferences?from&to&status&role&organizationId&cursor&limit`.
 *
 *  - Every visible range is ONE agenda query whose opaque `nextCursor` is followed until the
 *    last page (bounded); entries are keyed by `(meetingSpaceId, occurrenceKey)`, which stays
 *    stable when a recurring occurrence is materialized.
 *  - Day / week / month ranges are local dates in the *viewer's* zone, converted to UTC
 *    instants with the backend's DST rules; `to` is exclusive (no `T23:59:59` truncation).
 *  - An entry with `conferenceId: null` is a recurring occurrence beyond the horizon: it is
 *    shown, but join / start / conference-level reschedule need the conference; single
 *    occurrence changes go through `PUT /meeting-spaces/{id}/recurrence/exceptions`.
 *
 * Pure: no React, no network. Tested in tests/meetings-calendar.test.mjs.
 */
import type { AgendaItem, AgendaQuery, AgendaResponse, ConferenceStatus, Role } from "../types/generated/meeting-service";
import { utcToZonedLocal, zonedLocalToUtc } from "./meeting-time";

export type CalendarView = "day" | "week" | "month" | "agenda";

export const AGENDA_PAGE_LIMIT = 200;
/** Hard stop when following cursors for one visible range (200 × 10 = 2,000 occurrences). */
export const AGENDA_MAX_PAGES = 10;
/** The agenda view lists this many days from the anchor. */
export const AGENDA_VIEW_DAYS = 31;

const DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

function parseDate(date: string) {
  const match = DATE.exec(date);
  if (!match) throw new Error(`Invalid calendar date: ${date}`);
  return { y: +match[1], m: +match[2], d: +match[3] };
}

/** Calendar arithmetic on `YYYY-MM-DD` (zone-free). */
export function addDays(date: string, days: number): string {
  const { y, m, d } = parseDate(date);
  return new Date(Date.UTC(y, m - 1, d + days)).toISOString().slice(0, 10);
}

export function addMonths(date: string, months: number): string {
  const { y, m, d } = parseDate(date);
  const first = new Date(Date.UTC(y, m - 1 + months, 1));
  const last = new Date(Date.UTC(first.getUTCFullYear(), first.getUTCMonth() + 1, 0)).getUTCDate();
  return new Date(Date.UTC(first.getUTCFullYear(), first.getUTCMonth(), Math.min(d, last))).toISOString().slice(0, 10);
}

/** 0 = Sunday … 6 = Saturday. */
export function weekday(date: string): number {
  const { y, m, d } = parseDate(date);
  return new Date(Date.UTC(y, m - 1, d)).getUTCDay();
}

export function startOfWeek(date: string, weekStartsOn = 1): string {
  return addDays(date, -((weekday(date) - weekStartsOn + 7) % 7));
}

/** Today's date in a zone. */
export function todayIn(timeZone: string, now = Date.now()): string {
  return utcToZonedLocal(new Date(now).toISOString(), timeZone).slice(0, 10);
}

/** First and last-exclusive local dates of a view around an anchor date. */
export function viewDates(view: CalendarView, anchor: string, weekStartsOn = 1): { start: string; end: string } {
  switch (view) {
    case "day":
      return { start: anchor, end: addDays(anchor, 1) };
    case "week": {
      const start = startOfWeek(anchor, weekStartsOn);
      return { start, end: addDays(start, 7) };
    }
    case "month": {
      // The visible grid: whole weeks covering the month.
      const first = `${anchor.slice(0, 7)}-01`;
      const start = startOfWeek(first, weekStartsOn);
      const nextMonth = addMonths(first, 1);
      const end = addDays(startOfWeek(addDays(nextMonth, -1), weekStartsOn), 7);
      return { start, end };
    }
    case "agenda":
      return { start: anchor, end: addDays(anchor, AGENDA_VIEW_DAYS) };
  }
}

/** The UTC `[from, to)` of local dates `[start, end)` in a zone (DST-correct midnights). */
export function dateRangeToUtc(start: string, end: string, timeZone: string): { from: string; to: string } {
  return { from: zonedLocalToUtc(`${start}T00:00`, timeZone), to: zonedLocalToUtc(`${end}T00:00`, timeZone) };
}

export function viewRange(view: CalendarView, anchor: string, timeZone: string, weekStartsOn = 1) {
  const dates = viewDates(view, anchor, weekStartsOn);
  return { ...dates, ...dateRangeToUtc(dates.start, dates.end, timeZone) };
}

/** Next / previous anchor for the navigation buttons. */
export function shiftAnchor(view: CalendarView, anchor: string, direction: 1 | -1): string {
  switch (view) {
    case "day": return addDays(anchor, direction);
    case "week": return addDays(anchor, 7 * direction);
    case "month": return addMonths(anchor, direction);
    case "agenda": return addDays(anchor, AGENDA_VIEW_DAYS * direction);
  }
}

/** Local dates `[start, end)`, e.g. the cells of a week or month grid. */
export function datesBetween(start: string, end: string): string[] {
  const out: string[] = [];
  for (let date = start; date < end; date = addDays(date, 1)) out.push(date);
  return out;
}

export function agendaItemKey(item: Pick<AgendaItem, "meetingSpaceId" | "occurrenceKey">): string {
  return `${item.meetingSpaceId}|${item.occurrenceKey}`;
}

function compareItems(a: AgendaItem, b: AgendaItem): number {
  const ta = Date.parse(a.scheduledStartAt), tb = Date.parse(b.scheduledStartAt);
  if (ta !== tb) return ta - tb;
  if (a.meetingSpaceId !== b.meetingSpaceId) return a.meetingSpaceId < b.meetingSpaceId ? -1 : 1;
  return a.occurrenceKey < b.occurrenceKey ? -1 : a.occurrenceKey > b.occurrenceKey ? 1 : 0;
}

/**
 * Merges pages in order. A later copy of the same `(space, occurrenceKey)` replaces the
 * earlier one (e.g. it was materialized between page reads); the backend order
 * `(scheduledStartAt, meetingSpaceId, occurrenceKey)` is kept.
 */
export function mergeAgendaPages(pages: readonly Pick<AgendaResponse, "items">[]): AgendaItem[] {
  const byKey = new Map<string, AgendaItem>();
  for (const page of pages) for (const item of page.items) byKey.set(agendaItemKey(item), item);
  return [...byKey.values()].sort(compareItems);
}

export interface AgendaRangeResult {
  items: AgendaItem[];
  /** True when the page cap stopped the walk before the last page. */
  truncated: boolean;
  pages: number;
}

/**
 * Reads one visible range by following `nextCursor`. The cursor is opaque: it is passed back
 * unchanged with the same filters. A repeated cursor stops the walk (defensive).
 */
export async function fetchAgendaRange(
  fetchPage: (query: AgendaQuery) => Promise<AgendaResponse>,
  query: Omit<AgendaQuery, "cursor">,
  { maxPages = AGENDA_MAX_PAGES, limit = AGENDA_PAGE_LIMIT }: { maxPages?: number; limit?: number } = {},
): Promise<AgendaRangeResult> {
  const pages: AgendaResponse[] = [];
  const seen = new Set<string>();
  let cursor: string | null | undefined;
  do {
    const page = await fetchPage({ ...query, limit, ...(cursor ? { cursor } : {}) });
    pages.push(page);
    cursor = page.nextCursor ?? null;
    if (cursor && seen.has(cursor)) cursor = null;
    if (cursor) seen.add(cursor);
  } while (cursor && pages.length < maxPages);
  return { items: mergeAgendaPages(pages), truncated: !!cursor, pages: pages.length };
}

/** Query-string params for the agenda request; empty filters are omitted. */
export function agendaParams(query: AgendaQuery): Record<string, string | number> {
  const out: Record<string, string | number> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") continue;
    out[key] = value as string | number;
  }
  return out;
}

/** Entries grouped by their local start date in `timeZone` (ordered). */
export function groupByLocalDate(items: readonly AgendaItem[], timeZone: string): Map<string, AgendaItem[]> {
  const groups = new Map<string, AgendaItem[]>();
  for (const item of items) {
    const date = utcToZonedLocal(item.scheduledStartAt, timeZone).slice(0, 10);
    const list = groups.get(date);
    if (list) list.push(item); else groups.set(date, [item]);
  }
  return groups;
}

/** Minutes since local midnight and duration (start-only occurrences last 60 minutes). */
export function dayPlacement(item: Pick<AgendaItem, "scheduledStartAt" | "scheduledEndAt">, timeZone: string) {
  const local = utcToZonedLocal(item.scheduledStartAt, timeZone);
  const startMinute = +local.slice(11, 13) * 60 + +local.slice(14, 16);
  const end = item.scheduledEndAt ? Date.parse(item.scheduledEndAt) : Date.parse(item.scheduledStartAt) + 60 * 60_000;
  const duration = Math.max(15, Math.round((end - Date.parse(item.scheduledStartAt)) / 60_000));
  return { date: local.slice(0, 10), startMinute, durationMinutes: Math.min(duration, 24 * 60 - startMinute) };
}

const UNSTARTED: ConferenceStatus[] = ["CREATED", "PREPARING", "WAITING"];
const SCHEDULER_ROLES: Role[] = ["OWNER", "HOST"];

export interface OccurrenceActions {
  /** Open the pre-join for this conference (`/conferences/{id}`). */
  join: boolean;
  /** `POST /conferences/{id}/reschedule` (CREATED only, FULL view, owner/host). */
  reschedule: boolean;
  /** `POST /conferences/{id}/cancel` (unstarted, FULL view, owner/host). */
  cancel: boolean;
  /** `PUT /meeting-spaces/{id}/recurrence/exceptions` for an occurrence not yet materialized. */
  exception: boolean;
  /** The occurrence belongs to a recurring series (series edits live on the space page). */
  recurring: boolean;
  /** Download `GET /conferences/{id}/calendar.ics`. */
  ics: boolean;
  /** The original series start, for an exception request. */
  originalStartAt: string | null;
}

/**
 * Controls an agenda entry may show. Only what the agenda says the caller is permitted is
 * offered (owner/host in FULL view); a CO_HOST's scheduling rights depend on the space role
 * policy, which the agenda does not carry, so CO_HOSTs manage from the space page.
 * The backend re-checks every action.
 */
export function occurrenceActions(item: AgendaItem): OccurrenceActions {
  const recurring = item.occurrenceKey.startsWith("rrule:");
  const manager = item.view === "FULL" && !!item.viewerRole && SCHEDULER_ROLES.includes(item.viewerRole);
  const activeSpace = item.meetingSpace.status === "ACTIVE";
  const materialized = item.materialized && !!item.conferenceId;
  const originalStartAt = item.originalStartAt ?? (recurring && !item.isException ? item.scheduledStartAt : null);
  return {
    join: materialized && activeSpace && [...UNSTARTED, "LIVE"].includes(item.status),
    reschedule: materialized && manager && activeSpace && item.status === "CREATED",
    cancel: materialized && manager && activeSpace && UNSTARTED.includes(item.status),
    exception: !materialized && recurring && manager && activeSpace && !!originalStartAt && item.status !== "CANCELLED",
    recurring,
    ics: materialized,
    originalStartAt,
  };
}

/** Relations shown as filters (`role=`). */
export const AGENDA_RELATIONS = ["OWNER", "MEMBER", "INVITEE", "REGISTRANT", "PARTICIPANT", "ORGANIZATION"] as const;
