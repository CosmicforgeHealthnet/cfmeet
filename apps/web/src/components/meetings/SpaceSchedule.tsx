"use client";

/**
 * MTG-004: a space card shows its canonical conference (`latestConference`: LIVE > next
 * unstarted occurrence > latest) at the scheduled time in the meeting timezone and the
 * viewer's zone — never the space `createdAt`. Healthcare / high-security summaries carry no
 * timing, so only the status is shown.
 */
import { useState } from "react";
import { CalendarClock, Radio } from "lucide-react";
import { scheduleSummary, viewerTimeZone } from "@/lib/meeting-time";
import type { MeetingSpaceResponse } from "@/types/v2";

export default function SpaceSchedule({ space, compact = false }: { space: Pick<MeetingSpaceResponse, "latestConference" | "timezone">; compact?: boolean }) {
  // Read once per mount: "passed" vs "upcoming" does not need a live clock on a list card.
  const [now] = useState(() => Date.now());
  const summary = scheduleSummary(space.latestConference ?? null, space.timezone ?? undefined, now, viewerTimeZone());
  const live = summary.state === "LIVE";
  return <span className={`flex min-w-0 items-start gap-1 ${live ? "text-emerald-700 dark:text-emerald-400" : ""}`} suppressHydrationWarning>
    {live ? <Radio className="mt-0.5 size-3.5 shrink-0" /> : <CalendarClock className="mt-0.5 size-3.5 shrink-0" />}
    <span className="min-w-0">
      <span className={`block ${compact ? "truncate" : ""}`}>{summary.primary ? (summary.state === "UPCOMING" || summary.state === "PAST" ? summary.primary : `${summary.label} · ${summary.primary}`) : summary.label}</span>
      {summary.secondary && !compact ? <span className="block text-[11px] text-muted-foreground">{summary.secondary}</span> : null}
    </span>
  </span>;
}
