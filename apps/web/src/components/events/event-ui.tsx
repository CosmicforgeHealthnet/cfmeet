"use client";

/** Small pieces shared by the Events screens: time labels, the cover, type badges, people names. */
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { CalendarDays, MapPin, Phone, Video } from "lucide-react";
import { cn } from "@/lib/utils";
import { eventsService } from "@/services/v2/events.service";
import { useSharedMessagingEngine } from "@/components/messaging/MessagingEngineProvider";
import { localDate, type CalendarEvent, type EventType } from "@/lib/events/core";
import { zoneAbbreviation } from "@/lib/meeting-time";

export const field = "h-11 w-full rounded-xl border border-transparent bg-muted/70 px-3 text-sm text-foreground placeholder:text-muted-foreground focus:border-primary focus:bg-background focus:outline-none focus:ring-2 focus:ring-primary/20 disabled:opacity-50";
export const sectionTitle = "text-xs font-semibold uppercase tracking-wider text-muted-foreground";

export const TYPE_ICON: Record<EventType, typeof Video> = { ONLINE: Video, PHYSICAL: MapPin, CALL: Phone };
export const TYPE_LABEL: Record<EventType, string> = { ONLINE: "Online", PHYSICAL: "In person", CALL: "Call" };

const fmt = (iso: string, zone: string, options: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(undefined, { ...options, timeZone: zone }).format(new Date(iso));
const DAY: Intl.DateTimeFormatOptions = { weekday: "short", day: "numeric", month: "short" };
const DAY_YEAR: Intl.DateTimeFormatOptions = { weekday: "short", day: "numeric", month: "short", year: "numeric" };
const TIME: Intl.DateTimeFormatOptions = { hour: "numeric", minute: "2-digit" };

/** "Fri, 10 Jul, 7:00 PM – 9:00 PM" in `zone` (all day: the event's own dates). */
export function whenText(startsAt: string, endsAt: string, allDay: boolean, zone: string, withYear = false): string {
  const dayFmt = withYear ? DAY_YEAR : DAY;
  if (allDay) {
    const first = fmt(startsAt, zone, dayFmt);
    const lastIso = new Date(Date.parse(endsAt) - 1).toISOString();
    const last = fmt(lastIso, zone, dayFmt);
    return first === last ? `${first} · All day` : `${first} – ${last} · All day`;
  }
  const sameDay = localDate(startsAt, zone) === localDate(endsAt, zone);
  return sameDay
    ? `${fmt(startsAt, zone, dayFmt)}, ${fmt(startsAt, zone, TIME)} – ${fmt(endsAt, zone, TIME)}`
    : `${fmt(startsAt, zone, { ...dayFmt, ...TIME })} – ${fmt(endsAt, zone, { ...dayFmt, ...TIME })}`;
}

/** The time in the viewer's zone, and in the event's zone when it differs. */
export function WhenLines({ startsAt, endsAt, event, viewerZone, className, withYear }: { startsAt: string; endsAt: string; event: Pick<CalendarEvent, "allDay" | "timeZone">; viewerZone: string; className?: string; withYear?: boolean }) {
  const zone = event.allDay ? event.timeZone : viewerZone;
  const primary = whenText(startsAt, endsAt, event.allDay, zone, withYear);
  const differs = !event.allDay && event.timeZone && event.timeZone !== viewerZone && zoneAbbreviation(event.timeZone, startsAt) !== zoneAbbreviation(viewerZone, startsAt);
  return (
    <span className={className}>
      <span className="block">{primary}{differs ? <span className="text-muted-foreground"> (your time)</span> : null}</span>
      {differs ? <span className="block text-xs text-muted-foreground">{whenText(startsAt, endsAt, false, event.timeZone, withYear)} {zoneAbbreviation(event.timeZone, startsAt)} · {event.timeZone.replaceAll("_", " ")}</span> : null}
    </span>
  );
}

/** A date heading: "Today", "Tomorrow", "Yesterday" or "Saturday, 28 November". */
export function dayHeading(date: string, today: string): string {
  const d = new Date(`${date}T12:00:00Z`);
  const t = new Date(`${today}T12:00:00Z`);
  const diff = Math.round((d.getTime() - t.getTime()) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Tomorrow";
  if (diff === -1) return "Yesterday";
  return new Intl.DateTimeFormat(undefined, { weekday: "long", day: "numeric", month: "long", ...(d.getUTCFullYear() !== t.getUTCFullYear() ? { year: "numeric" } : {}), timeZone: "UTC" }).format(d);
}

/** The cover photo (fetched with a signed, short-lived grant), or a tinted placeholder. */
export function EventCover({ mediaId, className, alt = "" }: { mediaId: string | null; className?: string; alt?: string }) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!mediaId) return;
    let active = true;
    eventsService.coverUrl(mediaId).then((u) => { if (active) setUrl(u); }, () => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [mediaId]);
  if (!mediaId || failed || !url) {
    return (
      <div className={cn("flex items-center justify-center bg-gradient-to-br from-primary/15 to-primary/5 text-primary", className)} aria-hidden>
        <CalendarDays className="size-8 opacity-70" />
      </div>
    );
  }
  // eslint-disable-next-line @next/next/no-img-element
  return <img src={url} alt={alt} className={cn("object-cover", className)} />;
}

export function TypeBadge({ type, className }: { type: EventType; className?: string }) {
  const Icon = TYPE_ICON[type];
  return <span className={cn("inline-flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground", className)}><Icon className="size-3" aria-hidden />{TYPE_LABEL[type]}</span>;
}

const NO_SNAPSHOT = () => null;
const NO_SUBSCRIBE = () => () => {};

/** The app-wide messaging snapshot (chats, names, me), or null outside the signed-in app. */
export function useMessagingSnapshot() {
  const engine = useSharedMessagingEngine();
  const s = useSyncExternalStore(engine ? engine.subscribe : NO_SUBSCRIBE, engine ? engine.getSnapshot : NO_SNAPSHOT, engine ? engine.getSnapshot : NO_SNAPSHOT);
  return { engine, snapshot: s };
}

/**
 * Names of principals the way the chats show them: the messaging engine's names first, then
 * the names API (contacts, colleagues and members of `conversationIds`).
 */
export function usePrincipalNames(principalIds: string[], conversationIds: string[] = []) {
  const { snapshot } = useMessagingSnapshot();
  const known = snapshot?.names ?? {};
  const me = snapshot?.identity?.principalId ?? null;
  const missing = useMemo(() => [...new Set(principalIds)].filter((p) => p && !known[p]).sort(), [principalIds, known]);
  const lookup = useQuery({
    queryKey: ["v2", "messaging", "events", "names", missing, conversationIds],
    queryFn: () => eventsService.names(missing, conversationIds.filter(Boolean)),
    enabled: missing.length > 0,
    staleTime: 5 * 60_000,
  });
  return (principalId: string | null | undefined, fallback = "CF Meet user") => {
    if (!principalId) return fallback;
    if (principalId === me) return "You";
    return known[principalId] || lookup.data?.get(principalId) || fallback;
  };
}
