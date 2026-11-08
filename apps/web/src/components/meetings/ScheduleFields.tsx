"use client";

/**
 * Shared scheduling inputs and labels (MTG-012): times are typed and shown in the meeting
 * timezone, with the viewer's local time next to them when it differs.
 */
import { useMemo } from "react";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { localTimeKind, scheduleWindowLabel, validTimeZone, viewerTimeZone, zoneAbbreviation } from "@/lib/meeting-time";

const FALLBACK_ZONES = ["UTC", "Africa/Lagos", "Europe/London", "Europe/Rome", "America/New_York", "America/Chicago", "America/Los_Angeles", "Asia/Dubai", "Asia/Kolkata", "Asia/Tokyo", "Australia/Sydney"];

function allZones(): string[] {
  try {
    const supported = (Intl as unknown as { supportedValuesOf?: (key: string) => string[] }).supportedValuesOf?.("timeZone");
    if (supported?.length) return supported.includes("UTC") ? supported : ["UTC", ...supported];
  } catch {
    /* older engines */
  }
  return FALLBACK_ZONES;
}

export function TimeZoneField({ id, label = "Meeting timezone", value, onChange, disabled }: { id: string; label?: string; value: string; onChange: (zone: string) => void; disabled?: boolean }) {
  const zones = useMemo(() => {
    const list = allZones();
    return value && !list.includes(value) ? [value, ...list] : list;
  }, [value]);
  return <div className="grid gap-2">
    <Label htmlFor={id}>{label}</Label>
    <select id={id} className="h-10 rounded-md border border-input bg-background px-3 text-sm" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
      {zones.map((zone) => <option key={zone} value={zone}>{zone.replaceAll("_", " ")}</option>)}
    </select>
    {!validTimeZone(value) ? <p role="alert" className="text-xs text-destructive">Choose a valid IANA timezone.</p> : null}
  </div>;
}

/**
 * `datetime-local` read as wall time in `timeZone`. Warns about a spring-forward gap (the
 * time is moved forward by the gap) and a fall-back overlap (the first occurrence is used).
 */
export function ZonedDateTimeField({ id, label, value, onChange, timeZone, required, min, disabled, recurringStart = false }: {
  id: string; label: string; value: string; onChange: (value: string) => void; timeZone: string;
  required?: boolean; min?: string; disabled?: boolean; recurringStart?: boolean;
}) {
  let kind: ReturnType<typeof localTimeKind> = "ok";
  if (value && validTimeZone(timeZone)) {
    try { kind = localTimeKind(value, timeZone); } catch { kind = "ok"; }
  }
  return <div className="grid gap-2">
    <Label htmlFor={id}>{label} <span className="font-normal text-muted-foreground">({timeZone.replaceAll("_", " ")})</span></Label>
    <Input id={id} type="datetime-local" value={value} min={min} required={required} disabled={disabled} onChange={(event) => onChange(event.target.value)} />
    {kind === "nonexistent" ? <p className="text-xs text-amber-700 dark:text-amber-400">This time does not exist in {timeZone} (clocks spring forward). {recurringStart ? "A recurring series cannot start at it; choose another time." : "It will be scheduled one hour later."}</p> : null}
    {kind === "ambiguous" ? <p className="text-xs text-amber-700 dark:text-amber-400">This time occurs twice in {timeZone} (clocks fall back). {recurringStart ? "A recurring series cannot start at it; choose another time." : "The first occurrence is used."}</p> : null}
  </div>;
}

/** Start (– end) in the meeting zone, plus "(your time)" when the viewer's zone differs. */
export function ScheduleText({ start, end, timeZone, className }: { start: string; end?: string | null; timeZone?: string | null; className?: string }) {
  const label = scheduleWindowLabel(start, end, timeZone, viewerTimeZone());
  return <span className={className}>
    <span className="block">{label.primary}</span>
    {label.secondary ? <span className="block text-xs text-muted-foreground">{label.secondary}</span> : null}
  </span>;
}

export function ViewerZoneNote() {
  const zone = viewerTimeZone();
  return <span>{zone.replaceAll("_", " ")} ({zoneAbbreviation(zone)})</span>;
}
