/**
 * Meeting time semantics shared by lists, agenda, scheduling and pre-join.
 *
 * Backend contract (docs/operations/meetings-calendar-section-10.md):
 *  - every instant is RFC 3339 UTC; the meeting's IANA `timezone` is the display zone,
 *    shown together with the viewer's zone;
 *  - `datetime-local` input is wall time in the *space* timezone (not the browser zone);
 *  - a nonexistent local time (spring forward) occurs at the shifted instant
 *    (02:30 -> 03:30 EDT); a repeated local time (fall back) occurs at the first instant;
 *  - lists and dashboards show the scheduled start, never the space `createdAt`.
 */
import type { ConferenceStatus, ConferenceSummary } from "@/types/v2";

const LOCAL = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/;

export function validTimeZone(timeZone: string | undefined | null): timeZone is string {
  if (!timeZone) return false;
  try { new Intl.DateTimeFormat("en", { timeZone }).format(0); return true; } catch { return false; }
}

export function viewerTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

function wallParts(epochMs: number, timeZone: string) {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone, hourCycle: "h23", year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", second: "2-digit",
  }).formatToParts(new Date(epochMs));
  const get = (type: string) => Number(parts.find(part => part.type === type)?.value);
  return { year: get("year"), month: get("month"), day: get("day"), hour: get("hour") % 24, minute: get("minute"), second: get("second") };
}

/** Offset of `timeZone` from UTC at an instant, in milliseconds. */
function zoneOffset(epochMs: number, timeZone: string): number {
  const p = wallParts(epochMs, timeZone);
  const asUtc = Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second);
  return asUtc - Math.floor(epochMs / 1000) * 1000;
}

const pad = (value: number, size = 2) => String(value).padStart(size, "0");

/** The wall time of an instant in a zone, as `YYYY-MM-DDTHH:mm`. */
export function utcToZonedLocal(iso: string, timeZone: string): string {
  const p = wallParts(Date.parse(iso), timeZone);
  return `${pad(p.year, 4)}-${pad(p.month)}-${pad(p.day)}T${pad(p.hour)}:${pad(p.minute)}`;
}

/**
 * Converts wall time in `timeZone` to a UTC ISO instant with the backend's DST rules:
 * nonexistent wall times move forward by the gap, repeated wall times take the first instant.
 */
export function zonedLocalToUtc(local: string, timeZone: string): string {
  const match = LOCAL.exec(local);
  if (!match) throw new Error("Choose a date and time.");
  if (!validTimeZone(timeZone)) throw new Error("The meeting timezone is not a valid IANA timezone.");
  const [, y, mo, d, h, mi, s] = match;
  const guess = Date.UTC(+y, +mo - 1, +d, +h, +mi, s ? +s : 0);
  const day = 86_400_000;
  const before = zoneOffset(guess - day, timeZone);
  const offsets = new Set([before, zoneOffset(guess, timeZone), zoneOffset(guess + day, timeZone)]);
  offsets.add(zoneOffset(guess - zoneOffset(guess, timeZone), timeZone));
  const wanted = `${y}-${mo}-${d}T${h}:${mi}`;
  const candidates = [...offsets].map(offset => guess - offset)
    .filter(value => utcToZonedLocal(new Date(value).toISOString(), timeZone) === wanted)
    .sort((a, b) => a - b);
  // An existing wall time maps exactly (a repeated one takes the first instant); one in a
  // spring-forward gap keeps the pre-transition offset, i.e. moves forward by the gap.
  return new Date(candidates[0] ?? guess - before).toISOString();
}

/** "Mon, Nov 9, 2026, 9:00 AM" in the given zone. */
export function formatInZone(iso: string, timeZone: string, withZoneName = true): string {
  return new Intl.DateTimeFormat("en-US", {
    timeZone, weekday: "short", month: "short", day: "numeric", year: "numeric",
    hour: "numeric", minute: "2-digit", ...(withZoneName ? { timeZoneName: "short" as const } : {}),
  }).format(new Date(iso));
}

export function formatTimeInZone(iso: string, timeZone: string): string {
  return new Intl.DateTimeFormat("en-US", { timeZone, hour: "numeric", minute: "2-digit", timeZoneName: "short" }).format(new Date(iso));
}

/** The scheduled time in the meeting zone, plus the viewer zone when it differs. */
export function dualZoneLabel(iso: string, meetingZone: string | undefined, viewerZone = viewerTimeZone()) {
  const zone = validTimeZone(meetingZone) ? meetingZone : viewerZone;
  const primary = formatInZone(iso, zone);
  const sameInstantLabel = formatInZone(iso, viewerZone);
  const differs = zone !== viewerZone && zoneOffset(Date.parse(iso), zone) !== zoneOffset(Date.parse(iso), viewerZone);
  return { primary, secondary: differs ? `${sameInstantLabel} (your time)` : null, zone };
}

export type ScheduleState = "LIVE" | "UPCOMING" | "UNSCHEDULED" | "PAST" | "CLOSED" | "NONE";

const CLOSED: ConferenceStatus[] = ["ENDING", "ENDED", "CANCELLED", "FAILED"];

/**
 * What a meeting card shows for a space's canonical conference (`latestConference`:
 * LIVE > next unstarted occurrence > latest). Healthcare / high-security summaries carry
 * no timing at all; they show the status without a time.
 */
export function scheduleSummary(conference: ConferenceSummary | undefined | null, meetingZone?: string, now = Date.now(), viewerZone = viewerTimeZone()) {
  if (!conference) return { state: "NONE" as ScheduleState, label: "No occurrence scheduled", primary: null, secondary: null };
  if (conference.status === "LIVE") {
    const since = conference.startedAt ? dualZoneLabel(conference.startedAt, meetingZone, viewerZone) : null;
    return { state: "LIVE" as ScheduleState, label: "Live now", primary: since ? `Started ${since.primary}` : null, secondary: since?.secondary ?? null };
  }
  if (CLOSED.includes(conference.status)) {
    const at = conference.scheduledStartAt || conference.endedAt;
    const label = conference.status === "CANCELLED" ? "Cancelled" : "Ended";
    const time = at ? dualZoneLabel(at, meetingZone, viewerZone) : null;
    return { state: "CLOSED" as ScheduleState, label, primary: time?.primary ?? null, secondary: time?.secondary ?? null };
  }
  if (!conference.scheduledStartAt) return { state: "UNSCHEDULED" as ScheduleState, label: "Ready to start (no date)", primary: null, secondary: null };
  const time = dualZoneLabel(conference.scheduledStartAt, meetingZone, viewerZone);
  const past = Date.parse(conference.scheduledStartAt) < now;
  return { state: (past ? "PAST" : "UPCOMING") as ScheduleState, label: past ? "Scheduled start passed" : "Next occurrence", primary: time.primary, secondary: time.secondary };
}

/** [from, to) for a local calendar date range in the meeting zone; `to` is exclusive. */
export function zonedDateRange(fromDate: string, throughDate: string, timeZone: string) {
  const result: { from?: string; to?: string } = {};
  if (fromDate) result.from = zonedLocalToUtc(`${fromDate}T00:00`, timeZone);
  if (throughDate) {
    const [y, m, d] = throughDate.split("-").map(Number);
    const next = new Date(Date.UTC(y, m - 1, d + 1));
    result.to = zonedLocalToUtc(`${next.toISOString().slice(0, 10)}T00:00`, timeZone);
  }
  return result;
}
