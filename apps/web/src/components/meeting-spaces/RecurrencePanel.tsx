"use client";

import { useState } from "react";
import { CalendarX2, Repeat } from "lucide-react";
import type { MeetingSpaceResponse, RecurrenceResponse } from "@/types/v2";
import { useSpaceRecurrence, useSpaceExceptions, useSetRecurrence, useDeleteRecurrence, useSetException } from "@/hooks/v2/useSpaces";
import { recurrenceInput, conferencePermission } from "@/lib/meeting-scheduling";
import { formatInZone, localTimeKind, occurrenceWindowInZone, zonedLocalToUtc } from "@/lib/meeting-time";
import { spaceZone } from "@/components/conferences/ConferencesPanel";
import { ScheduleText, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";
import { cn } from "@/lib/utils";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

// ── Plain-language recurrence (RFC 5545 RRULE subset the builder can edit) ─────

type Freq = "DAILY" | "WEEKLY" | "MONTHLY";
const DAYS = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"] as const;
const DAY_SHORT: Record<string, string> = { MO: "M", TU: "T", WE: "W", TH: "T", FR: "F", SA: "S", SU: "S" };
const DAY_NAME: Record<string, string> = { MO: "Mon", TU: "Tue", WE: "Wed", TH: "Thu", FR: "Fri", SA: "Sat", SU: "Sun" };
type Rule = { freq: Freq; interval: number; byday: string[]; count?: number; until?: string };

function parseRule(rrule: string): Rule | null {
  const parts = Object.fromEntries(rrule.split(";").map((p) => p.split("=") as [string, string]));
  const allowed = new Set(["FREQ", "INTERVAL", "BYDAY", "COUNT", "UNTIL"]);
  if (Object.keys(parts).some((k) => !allowed.has(k))) return null;
  if (!["DAILY", "WEEKLY", "MONTHLY"].includes(parts.FREQ)) return null;
  const byday = parts.BYDAY ? parts.BYDAY.split(",") : [];
  if (byday.some((d) => !(DAYS as readonly string[]).includes(d))) return null;
  return {
    freq: parts.FREQ as Freq,
    interval: Number(parts.INTERVAL || 1),
    byday,
    count: parts.COUNT ? Number(parts.COUNT) : undefined,
    until: parts.UNTIL ? `${parts.UNTIL.slice(0, 4)}-${parts.UNTIL.slice(4, 6)}-${parts.UNTIL.slice(6, 8)}` : undefined,
  };
}

function buildRule(r: Rule): string {
  const out = [`FREQ=${r.freq}`];
  if (r.interval > 1) out.push(`INTERVAL=${r.interval}`);
  if (r.freq === "WEEKLY" && r.byday.length) out.push(`BYDAY=${DAYS.filter((d) => r.byday.includes(d)).join(",")}`);
  if (r.until) out.push(`UNTIL=${r.until.replaceAll("-", "")}T235959Z`);
  else out.push(`COUNT=${r.count ?? 10}`);
  return out.join(";");
}

function describeRule(rrule: string): string {
  const r = parseRule(rrule);
  if (!r) return "Custom repeating schedule";
  const unit = { DAILY: "day", WEEKLY: "week", MONTHLY: "month" }[r.freq];
  let text = r.interval > 1 ? `Every ${r.interval} ${unit}s` : `Every ${unit}`;
  if (r.freq === "WEEKLY" && r.byday.length) {
    const names = DAYS.filter((d) => r.byday.includes(d)).map((d) => DAY_NAME[d]);
    text += ` on ${names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names.at(-1)}` : names[0]}`;
  }
  if (r.until) text += `, until ${new Date(`${r.until}T00:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" })}`;
  else if (r.count) text += `, ${r.count} time${r.count === 1 ? "" : "s"}`;
  return text;
}

// ── Panel ─────────────────────────────────────────────────────────────────────

export default function RecurrencePanel({ space }: { space: MeetingSpaceResponse }) {
  const zone = spaceZone(space);
  const rule = useSpaceRecurrence(space.id);
  const exceptions = useSpaceExceptions(rule.data ? space.id : "");
  const remove = useDeleteRecurrence(space.id);
  const canManage = conferencePermission(space, "start");
  return <section className="space-y-6">
    <div>
      <h2 className="text-base font-semibold text-foreground">Repeating schedule</h2>
      <p className="text-sm text-muted-foreground">Repeats follow {zone.replaceAll("_", " ")} time, including daylight saving changes.</p>
    </div>

    {rule.isLoading && <div aria-busy="true" className="h-32 animate-pulse rounded-2xl bg-muted" />}
    {rule.isError && <RequestError error={rule.error} retry={() => void rule.refetch()} />}
    {rule.isSuccess && <>
      <div className="rounded-2xl border border-border bg-card p-5">
        {rule.data ? (
          <div className="flex items-start gap-3">
            <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><Repeat className="size-5" /></span>
            <div className="min-w-0">
              <p className="font-semibold text-foreground">{describeRule(rule.data.rrule)}</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Starts {new Date(rule.data.startLocal).toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })} · {rule.data.durationMinutes} min each
              </p>
              {rule.data.materializedUntil ? <p className="mt-0.5 text-xs text-muted-foreground">Sessions created up to {formatInZone(rule.data.materializedUntil, rule.data.timezone)}</p> : null}
            </div>
          </div>
        ) : (
          <div className="flex items-start gap-3">
            <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground"><Repeat className="size-5" /></span>
            <div><p className="font-semibold text-foreground">Doesn&apos;t repeat</p><p className="mt-1 text-sm text-muted-foreground">Make this a regular meeting, like every Monday at 10.</p></div>
          </div>
        )}
        {canManage && <div className="mt-4 flex flex-wrap gap-2 border-t border-border pt-4">
          <RecurrenceEditor key={rule.data?.id || "new"} space={space} rule={rule.data} />
          {rule.data && <ActionDialog label="Stop repeating" destructive description="Stop the repeating series. Future sessions that haven't started are removed, including moved ones. Past sessions are kept." submit={() => remove.mutateAsync()} />}
        </div>}
      </div>

      {rule.data && <div className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h3 className="text-sm font-semibold text-foreground">Changed dates</h3>
          {canManage && <ExceptionEditor spaceId={space.id} zone={rule.data.timezone || zone} />}
        </div>
        {exceptions.isLoading && <div aria-busy="true" className="h-16 animate-pulse rounded-2xl bg-muted" />}
        {exceptions.isError && <RequestError error={exceptions.error} retry={() => void exceptions.refetch()} />}
        {exceptions.data?.length === 0 && <p className="rounded-xl bg-muted/50 px-4 py-3 text-sm text-muted-foreground">No dates changed. Every session follows the schedule.</p>}
        {exceptions.data?.map(item => (
          <article key={item.id} className="flex items-start gap-3 rounded-2xl border border-border bg-card p-4 text-sm">
            <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-lg", item.kind === "CANCELLED" ? "bg-destructive/10 text-destructive" : "bg-amber-500/10 text-amber-600")}><CalendarX2 className="size-4" /></span>
            <div>
              <p className="font-medium text-foreground">{item.kind === "CANCELLED" ? "Cancelled" : "Moved"}: {new Date(item.originalStartLocal).toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })}</p>
              {item.newStartAt && <div className="mt-0.5 text-muted-foreground">New time: <ScheduleText start={item.newStartAt} end={item.newEndAt} timeZone={item.timezone} /></div>}
            </div>
          </article>
        ))}
      </div>}
    </>}
  </section>;
}

function Segmented<T extends string>({ value, options, onChange, label }: { value: T; options: { value: T; label: string }[]; onChange: (v: T) => void; label: string }) {
  return <div className="grid gap-2"><span className="text-sm font-medium text-foreground">{label}</span>
    <div className="grid rounded-xl bg-muted p-1" style={{ gridTemplateColumns: `repeat(${options.length}, minmax(0, 1fr))` }} role="radiogroup" aria-label={label}>
      {options.map(o => <button key={o.value} type="button" role="radio" aria-checked={value === o.value} onClick={() => onChange(o.value)} className={cn("h-9 rounded-lg text-sm font-medium transition-colors", value === o.value ? "bg-card text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>{o.label}</button>)}
    </div>
  </div>;
}

function RecurrenceEditor({ space, rule }: { space: MeetingSpaceResponse; rule: RecurrenceResponse | null | undefined }) {
  const zone = spaceZone(space);
  const parsed = rule ? parseRule(rule.rrule) : { freq: "WEEKLY" as Freq, interval: 1, byday: ["MO"], count: 10 };
  const [advanced, setAdvanced] = useState(!parsed);
  const [r, setR] = useState<Rule>(parsed ?? { freq: "WEEKLY", interval: 1, byday: ["MO"], count: 10 });
  const [ends, setEnds] = useState<"COUNT" | "UNTIL">(parsed?.until ? "UNTIL" : "COUNT");
  const [raw, setRaw] = useState(rule?.rrule || "FREQ=WEEKLY;BYDAY=MO;COUNT=10");
  const [local, setLocal] = useState(rule?.startLocal.slice(0, 16) || "");
  const [duration, setDuration] = useState(rule?.durationMinutes || 60);
  const save = useSetRecurrence(space.id);
  const effective: Rule = { ...r, ...(ends === "COUNT" ? { until: undefined, count: r.count || 10 } : { count: undefined }) };
  const rrule = advanced ? raw : buildRule(effective);

  return <ActionDialog label={rule ? "Edit schedule" : "Make it repeat"} title={rule ? "Edit repeating schedule" : "Repeat this meeting"} description={rule ? "Changes apply to future sessions. Dates you changed by hand keep their changes; past sessions are never touched." : "Pick how often it happens and when it stops."} submit={async () => {
    if (local && localTimeKind(local, zone) !== "ok") throw new Error("A series can't start at a time skipped or repeated by a daylight saving change.");
    if (!advanced && effective.freq === "WEEKLY" && effective.byday.length === 0) throw new Error("Pick at least one day.");
    if (!advanced && ends === "UNTIL" && !effective.until) throw new Error("Pick the end date.");
    await save.mutateAsync(recurrenceInput(rrule, local, duration));
  }}>
    <ZonedDateTimeField id={`series-start-${space.id}`} label="First meeting" required recurringStart value={local} onChange={setLocal} timeZone={zone} />
    <Field label="Length (minutes)" type="number" min="1" max="1440" required value={duration} onChange={e => setDuration(Number(e.target.value))} />

    {advanced ? <>
      <Field label="Rule (RFC 5545)" required value={raw} onChange={e => setRaw(e.target.value)} placeholder="FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10" />
      <p className="text-xs text-muted-foreground">For rules the simple editor can&apos;t express. Include COUNT or UNTIL so the series ends.</p>
    </> : <>
      <Segmented label="Repeats" value={r.freq} onChange={freq => setR({ ...r, freq })} options={[{ value: "DAILY", label: "Daily" }, { value: "WEEKLY", label: "Weekly" }, { value: "MONTHLY", label: "Monthly" }]} />
      <label className="flex items-center gap-3 text-sm text-foreground">Every
        <input type="number" min={1} max={30} value={r.interval} onChange={e => setR({ ...r, interval: Math.max(1, Number(e.target.value) || 1) })} className="h-10 w-20 rounded-xl border border-border bg-background px-3 text-sm" />
        {{ DAILY: "day", WEEKLY: "week", MONTHLY: "month" }[r.freq]}{r.interval > 1 ? "s" : ""}
      </label>
      {r.freq === "WEEKLY" && <div className="grid gap-2"><span className="text-sm font-medium text-foreground">On</span>
        <div className="flex gap-1.5">{DAYS.map(d => {
          const on = r.byday.includes(d);
          return <button key={d} type="button" aria-pressed={on} aria-label={DAY_NAME[d]} onClick={() => setR({ ...r, byday: on ? r.byday.filter(x => x !== d) : [...r.byday, d] })} className={cn("size-10 rounded-full text-sm font-semibold transition-colors", on ? "bg-primary text-primary-foreground" : "bg-muted text-muted-foreground hover:text-foreground")}>{DAY_SHORT[d]}</button>;
        })}</div>
      </div>}
      <Segmented label="Ends" value={ends} onChange={setEnds} options={[{ value: "COUNT", label: "After" }, { value: "UNTIL", label: "On a date" }]} />
      {ends === "COUNT"
        ? <label className="flex items-center gap-3 text-sm text-foreground"><input type="number" min={1} max={500} value={r.count ?? 10} onChange={e => setR({ ...r, count: Math.max(1, Number(e.target.value) || 1) })} className="h-10 w-24 rounded-xl border border-border bg-background px-3 text-sm" /> meetings</label>
        : <Field label="Last date" type="date" value={r.until ?? ""} onChange={e => setR({ ...r, until: e.target.value })} />}
      <p className="rounded-xl bg-primary/5 px-3 py-2.5 text-sm font-medium text-primary">{describeRule(rrule)}</p>
    </>}
    <button type="button" onClick={() => setAdvanced(a => !a)} className="text-xs font-medium text-muted-foreground underline-offset-2 hover:underline">{advanced ? "Use the simple editor" : "Advanced: enter a rule"}</button>
  </ActionDialog>;
}

function ExceptionEditor({ spaceId, zone }: { spaceId: string; zone: string }) {
  const save = useSetException(spaceId);
  const [original, setOriginal] = useState("");
  const [action, setAction] = useState<"CANCEL" | "RESCHEDULE">("CANCEL");
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  return <ActionDialog label="Change one date" title="Change one date" description="Cancel or move a single future session. Pick its usual date and time; every other session stays the same. You can also do this from the calendar." submit={() => save.mutateAsync({
    originalStartAt: zonedLocalToUtc(original, zone),
    action,
    ...(action === "RESCHEDULE" ? occurrenceWindowInZone(start, end || null, zone) : {}),
  })}>
    <ZonedDateTimeField id={`exception-original-${spaceId}`} label="Usual date and time" required value={original} onChange={setOriginal} timeZone={zone} />
    <Segmented label="What to do" value={action} onChange={setAction} options={[{ value: "CANCEL", label: "Cancel it" }, { value: "RESCHEDULE", label: "Move it" }]} />
    {action === "RESCHEDULE" && <>
      <ZonedDateTimeField id={`exception-start-${spaceId}`} label="New start" required value={start} onChange={setStart} timeZone={zone} />
      <ZonedDateTimeField id={`exception-end-${spaceId}`} label="New end (optional)" value={end} min={start} onChange={setEnd} timeZone={zone} />
    </>}
  </ActionDialog>;
}
