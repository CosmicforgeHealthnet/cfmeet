/**
 * Calendar / agenda presentation (FE-021 / FE-022, MTG-012 client side).
 *
 * The agenda comes from `GET /api/v2/me/conferences` (materialized conferences and projected
 * recurring occurrences). Times are instants (UTC); each item carries its space's time zone.
 * The app shows times in the device zone and, when the space zone differs, the space-local
 * time too, so "10:00" never silently means two different things.
 *
 * Pure (uses only `Intl` via lib/time/zoned); runs under `node --test`.
 */
import type { AgendaItem, ConferenceStatus, Role } from "@/lib/api/generated/meeting-service";
import { offsetAt, wallTimeAt, zoneLabel } from "../time/zoned.ts";

/**
 * Items of the selected workspace. Organization context: the server already filtered by
 * `organizationId`. Personal context: the agenda includes every meeting the user can see, so
 * organization meetings are separated out (they appear under that organization's workspace).
 */
export function itemsForWorkspace<T extends Pick<AgendaItem, "meetingSpace">>(
  items: T[],
  organizationId: string | undefined
): T[] {
  if (organizationId) return items.filter((i) => i.meetingSpace.organizationId === organizationId);
  return items.filter((i) => !i.meetingSpace.organizationId);
}

export interface DayGroup<T> {
  /** `YYYY-MM-DD` in the display zone. */
  day: string;
  items: T[];
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** `YYYY-MM-DD` of instant `epochMs` in `timeZone`. */
export function dayKey(epochMs: number, timeZone: string): string {
  const w = wallTimeAt(epochMs, timeZone);
  return `${w.year}-${pad(w.month)}-${pad(w.day)}`;
}

/** Groups items by their start day in `timeZone`, in chronological order. */
export function groupByDay<T extends Pick<AgendaItem, "scheduledStartAt">>(items: T[], timeZone: string): DayGroup<T>[] {
  const sorted = [...items].sort((a, b) => Date.parse(a.scheduledStartAt) - Date.parse(b.scheduledStartAt));
  const groups: DayGroup<T>[] = [];
  for (const item of sorted) {
    const day = dayKey(Date.parse(item.scheduledStartAt), timeZone);
    const last = groups[groups.length - 1];
    if (last && last.day === day) last.items.push(item);
    else groups.push({ day, items: [item] });
  }
  return groups;
}

/** "Today", "Tomorrow", or e.g. "Tue, Nov 10" for a day key relative to `nowMs`. */
export function dayLabel(day: string, nowMs: number, timeZone: string, locale?: string): string {
  const today = dayKey(nowMs, timeZone);
  const tomorrow = dayKey(nowMs + 86400000, timeZone);
  if (day === today) return "Today";
  if (day === tomorrow) return "Tomorrow";
  const [y, m, d] = day.split("-").map(Number);
  try {
    return new Intl.DateTimeFormat(locale, { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" }).format(
      new Date(Date.UTC(y, m - 1, d, 12))
    );
  } catch {
    return day;
  }
}

/** "HH:MM" (24 h) of an instant in a zone — locale-independent so tests are stable. */
export function clock(epochMs: number, timeZone: string): string {
  const w = wallTimeAt(epochMs, timeZone);
  return `${pad(w.hour)}:${pad(w.minute)}`;
}

export interface TimeDisplay {
  /** e.g. "14:00 – 15:00" in the device zone. */
  local: string;
  /** e.g. "09:00 – 10:00 EST (meeting time)" when the space zone differs; otherwise null. */
  spaceLocal: string | null;
}

/** Start/end in the device zone, plus the space-local time when the zones differ at that instant. */
export function describeTime(
  startIso: string,
  endIso: string | null | undefined,
  spaceTimeZone: string | null | undefined,
  deviceTimeZone: string
): TimeDisplay {
  const start = Date.parse(startIso);
  const end = endIso ? Date.parse(endIso) : null;
  const range = (tz: string) => (end !== null ? `${clock(start, tz)} – ${clock(end, tz)}` : clock(start, tz));
  const local = range(deviceTimeZone);
  if (!spaceTimeZone || spaceTimeZone === deviceTimeZone) return { local, spaceLocal: null };
  let differs: boolean;
  try {
    differs = offsetAt(start, spaceTimeZone) !== offsetAt(start, deviceTimeZone);
  } catch {
    return { local, spaceLocal: null };
  }
  if (!differs) return { local, spaceLocal: null };
  return { local, spaceLocal: `${range(spaceTimeZone)} ${zoneLabel(start, spaceTimeZone, "en-US")} (meeting time)` };
}

export type ConferenceAction = "join" | "start" | "none";

const HOST_ROLES: Role[] = ["OWNER", "HOST", "CO_HOST"];

/** Which primary action an agenda row / conference row offers. The server decides on press. */
export function conferenceAction(status: ConferenceStatus, viewerRole: Role | null | undefined): ConferenceAction {
  switch (status) {
    case "LIVE":
    case "PREPARING":
      return "join";
    case "CREATED":
    case "WAITING":
      return viewerRole && HOST_ROLES.includes(viewerRole) ? "start" : "join";
    default:
      return "none";
  }
}

export function statusLabel(status: ConferenceStatus): string {
  switch (status) {
    case "LIVE":
      return "Live";
    case "CREATED":
    case "WAITING":
    case "PREPARING":
      return "Scheduled";
    case "ENDING":
      return "Ending";
    case "ENDED":
      return "Ended";
    case "CANCELLED":
      return "Cancelled";
    case "FAILED":
      return "Failed";
    default:
      return status;
  }
}
