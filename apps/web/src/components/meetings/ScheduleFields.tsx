"use client";

/**
 * Shared scheduling inputs and labels (MTG-012): times are typed and shown in the meeting
 * timezone, with the viewer's local time next to them when it differs.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { Check, ChevronDown, Globe, Search } from "lucide-react";
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

/** "GMT+1" style offset for a zone, now. */
function zoneOffset(zone: string): string {
  try {
    const part = new Intl.DateTimeFormat("en-US", { timeZone: zone, timeZoneName: "shortOffset" })
      .formatToParts(new Date())
      .find((p) => p.type === "timeZoneName");
    return part?.value ?? "";
  } catch {
    return "";
  }
}

/** Searchable time zone picker (Google Calendar style): type a city, see each zone's offset. */
export function TimeZoneField({ id, label = "Meeting timezone", value, onChange, disabled }: { id: string; label?: string; value: string; onChange: (zone: string) => void; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const zones = useMemo(() => {
    const list = allZones();
    return value && !list.includes(value) ? [value, ...list] : list;
  }, [value]);
  const results = useMemo(() => {
    const q = query.trim().toLowerCase().replaceAll(" ", "_");
    return (q ? zones.filter((z) => z.toLowerCase().includes(q)) : zones).slice(0, 80);
  }, [zones, query]);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActive(0);
    requestAnimationFrame(() => searchRef.current?.focus());
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const pick = (zone: string | undefined) => {
    if (!zone) return;
    onChange(zone);
    setOpen(false);
  };

  return <div className="grid gap-2" ref={ref}>
    <Label htmlFor={id}>{label}</Label>
    <div className="relative">
      <button
        id={id}
        type="button"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex h-11 w-full items-center gap-2.5 rounded-xl border border-transparent bg-muted/70 focus:bg-background px-3 text-left text-sm text-foreground transition-colors hover:bg-muted/50 focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary disabled:opacity-50"
      >
        <Globe className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <span className="min-w-0 flex-1 truncate">{value.replaceAll("_", " ")}</span>
        <span className="shrink-0 text-xs text-muted-foreground">{zoneOffset(value)}</span>
        <ChevronDown className={`size-4 shrink-0 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`} aria-hidden />
      </button>
      {open && (
        <div className="absolute left-0 right-0 top-full z-40 mt-2 overflow-hidden rounded-xl border border-border bg-popover shadow-xl">
          <div className="flex items-center gap-2 border-b border-border px-3">
            <Search className="size-4 text-muted-foreground" aria-hidden />
            <input
              ref={searchRef}
              value={query}
              onChange={(e) => { setQuery(e.target.value); setActive(0); }}
              onKeyDown={(e) => {
                if (e.key === "Escape") setOpen(false);
                else if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => Math.min(a + 1, results.length - 1)); }
                else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
                else if (e.key === "Enter") { e.preventDefault(); pick(results[active]); }
              }}
              placeholder="Search city or region"
              aria-label="Search time zones"
              className="h-11 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
            />
          </div>
          <ul role="listbox" aria-label="Time zones" className="max-h-64 overflow-y-auto p-1.5">
            {results.length === 0 && <li className="px-3 py-4 text-center text-sm text-muted-foreground">No time zones match</li>}
            {results.map((zone, i) => (
              <li key={zone} role="option" aria-selected={zone === value}>
                <button
                  type="button"
                  onMouseEnter={() => setActive(i)}
                  onClick={() => pick(zone)}
                  className={`flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left text-sm ${i === active ? "bg-muted" : ""}`}
                >
                  <span className="min-w-0 flex-1 truncate text-foreground">{zone.replaceAll("_", " ")}</span>
                  <span className="shrink-0 text-xs text-muted-foreground">{zoneOffset(zone)}</span>
                  {zone === value && <Check className="size-4 shrink-0 text-primary" aria-hidden />}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
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
    <Input id={id} type="datetime-local" value={value} min={min} required={required} disabled={disabled} onChange={(event) => onChange(event.target.value)} className="h-11 rounded-xl px-3 py-0 text-sm sm:h-11 sm:px-3 sm:py-0 sm:text-sm" />
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
