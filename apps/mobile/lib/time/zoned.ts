/**
 * Wall-clock time in an IANA time zone <-> UTC instants (MTG-012 client side).
 *
 * The Meeting Service schedules in the meeting space's time zone: a form's date and time are
 * a wall-clock time *in the space time zone*, not in the phone's zone. Rules match the backend
 * (docs/operations/meetings-calendar-section-10.md):
 * - a nonexistent local time (spring forward) occurs at the shifted instant
 *   (02:30 → 03:30 EDT);
 * - a repeated local time (fall back) occurs at the first instant.
 *
 * Pure (uses only `Intl`); runs under `node --test`.
 */

export interface WallTime {
  year: number;
  /** 1-12 */
  month: number;
  day: number;
  hour: number;
  minute: number;
}

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(timeZone: string): Intl.DateTimeFormat {
  let f = formatters.get(timeZone);
  if (!f) {
    f = new Intl.DateTimeFormat("en-US", {
      timeZone,
      hourCycle: "h23",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    formatters.set(timeZone, f);
  }
  return f;
}

/** True when `timeZone` is an IANA zone this device's `Intl` knows. */
export function isValidTimeZone(timeZone: string): boolean {
  try {
    formatter(timeZone);
    return true;
  } catch {
    return false;
  }
}

/** The device's IANA time zone, or `UTC` when it cannot be determined. */
export function deviceTimeZone(): string {
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return tz && isValidTimeZone(tz) ? tz : "UTC";
  } catch {
    return "UTC";
  }
}

/** The wall-clock time of instant `epochMs` in `timeZone`. */
export function wallTimeAt(epochMs: number, timeZone: string): WallTime {
  const parts: Record<string, number> = {};
  for (const p of formatter(timeZone).formatToParts(new Date(epochMs))) {
    if (p.type !== "literal") parts[p.type] = Number(p.value);
  }
  return {
    year: parts.year,
    month: parts.month,
    day: parts.day,
    hour: parts.hour === 24 ? 0 : parts.hour,
    minute: parts.minute,
  };
}

function asUtc(w: WallTime): number {
  return Date.UTC(w.year, w.month - 1, w.day, w.hour, w.minute);
}

/** Offset of `timeZone` from UTC at instant `epochMs`, in milliseconds. */
export function offsetAt(epochMs: number, timeZone: string): number {
  const flooredToMinute = Math.floor(epochMs / 60000) * 60000;
  return asUtc(wallTimeAt(flooredToMinute, timeZone)) - flooredToMinute;
}

/**
 * The UTC instant of wall-clock time `wall` in `timeZone`.
 * Gap (nonexistent) → shifted forward by the gap; overlap (repeated) → the earlier instant.
 */
export function zonedToUtc(wall: WallTime, timeZone: string): number {
  const naive = asUtc(wall);
  // Candidate offsets: the zone's offset just before and just after the naive instant
  // (a day either side brackets any transition).
  const before = offsetAt(naive - 86400000, timeZone);
  const after = offsetAt(naive + 86400000, timeZone);
  const candidates = [naive - before, naive - after].filter(
    (instant) => asUtc(wallTimeAt(instant, timeZone)) === naive
  );
  if (candidates.length > 0) return Math.min(...candidates);
  // Nonexistent local time: interpret with the offset in force before the transition, which
  // lands the same distance past the transition (02:30 → 03:30 for a one-hour gap).
  return naive - before;
}

/** RFC 3339 UTC timestamp (`…Z`) without milliseconds. */
export function toIsoUtc(epochMs: number): string {
  return new Date(epochMs).toISOString().replace(/\.\d{3}Z$/, "Z");
}

/** Validates a schedule the way the backend does (start not >5 min past; end ≤24 h after). */
export function scheduleProblem(startMs: number, endMs: number | null, nowMs: number): string | null {
  if (startMs < nowMs - 5 * 60000) return "The start time is in the past.";
  if (endMs !== null) {
    if (endMs <= startMs) return "The end time must be after the start time.";
    if (endMs - startMs > 24 * 3600000) return "A meeting can last at most 24 hours.";
  }
  return null;
}

/** e.g. "Mon, Nov 9, 14:30" in `timeZone`. */
export function formatInZone(epochMs: number, timeZone: string, locale?: string): string {
  try {
    return new Intl.DateTimeFormat(locale, {
      timeZone,
      weekday: "short",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(epochMs));
  } catch {
    return new Date(epochMs).toISOString();
  }
}

/** Short zone label, e.g. "GMT+1" or "EST", for display next to a time. */
export function zoneLabel(epochMs: number, timeZone: string, locale?: string): string {
  try {
    const part = new Intl.DateTimeFormat(locale, { timeZone, timeZoneName: "short" })
      .formatToParts(new Date(epochMs))
      .find((p) => p.type === "timeZoneName");
    return part?.value ?? timeZone;
  } catch {
    return timeZone;
  }
}
