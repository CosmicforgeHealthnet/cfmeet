"use client";

/**
 * A CF Meet event link in a chat message, shown as a card (title and time read from the
 * event, which the server only answers to people who may see it). When the event isn't
 * available (404) nothing is added: the message keeps its plain link.
 */
import { useMemo } from "react";
import Link from "next/link";
import { CalendarDays } from "lucide-react";
import { cn } from "@/lib/utils";
import { viewerTimeZone } from "@/lib/meeting-time";
import { useEvent } from "@/hooks/v2/useEvents";
import { eventHref, eventIdsInText } from "@/lib/events/core";
import { whenText } from "./event-ui";

export function EventLinkCards({ text, mine }: { text: string | null; mine: boolean }) {
  const ids = useMemo(() => (typeof window === "undefined" ? [] : eventIdsInText(text, window.location.origin)), [text]);
  if (!ids.length) return null;
  return <>{ids.map((id) => <EventLinkCard key={id} eventId={id} mine={mine} />)}</>;
}

function EventLinkCard({ eventId, mine }: { eventId: string; mine: boolean }) {
  const event = useEvent(eventId);
  const zone = useMemo(() => viewerTimeZone(), []);
  if (!event.data) return null;
  const e = event.data;
  const occurrence = e.nextOccurrence ?? { startsAt: e.startsAt, endsAt: e.endsAt };
  const cancelled = e.state === "CANCELLED";
  return (
    <Link href={eventHref(e.eventId)} data-testid="event-card" onClick={(ev) => ev.stopPropagation()}
      className={cn("mt-1.5 flex max-w-xs items-center gap-3 rounded-xl p-2.5 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40",
        mine ? "bg-white/15 hover:bg-white/20" : "bg-muted hover:bg-muted/70")}>
      <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-lg", mine ? "bg-white/20" : "bg-primary/10 text-primary")}><CalendarDays className="size-5" aria-hidden /></span>
      <span className="min-w-0">
        <span className={cn("block truncate text-sm font-semibold", cancelled && "line-through opacity-80")}>{e.title}</span>
        <span className="block truncate text-xs opacity-80">{cancelled ? "Cancelled" : whenText(occurrence.startsAt, occurrence.endsAt, e.allDay, e.allDay ? e.timeZone : zone)}</span>
      </span>
    </Link>
  );
}
