"use client";

/**
 * One event (ADR 0071): when (viewer's zone and the event's), recurrence, next occurrences,
 * place, people, RSVP, the viewer's own reminders, joining (the linked CF Meet meeting, or
 * a call in the event's chat), sharing into a chat, the calendar file, and the organizers'
 * edit / cancel / delete. Anything the viewer may not see is a 404: "isn't available".
 */
import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Ban, CalendarClock, CalendarPlus, ChevronLeft, Download, MapPin, MessageSquare, Pencil, Phone, Repeat, Send, Trash2, UserPlus, Users, Video, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { viewerTimeZone } from "@/lib/meeting-time";
import { callHref } from "@/lib/communications/comms-core";
import { useEvent, useEventAttendees, useEventChanged, useRespond } from "@/hooks/v2/useEvents";
import { eventErrorMessage, eventsService, isNotFound, saveCalendarFile, type EventAttendee } from "@/services/v2/events.service";
import { LIMITS, eventShareText, meetingTarget, recurrenceSummary, localDate, type CalendarEvent, type RsvpResponse } from "@/lib/events/core";
import EventForm, { PeoplePicker, RemindersEditor } from "./EventForm";
import { EventCover, TypeBadge, WhenLines, sectionTitle, useMessagingSnapshot, usePrincipalNames } from "./event-ui";

const RSVP: { value: RsvpResponse; label: string; count: (c: CalendarEvent["counts"]) => number }[] = [
  { value: "GOING", label: "Going", count: (c) => c.going },
  { value: "MAYBE", label: "Maybe", count: (c) => c.maybe },
  { value: "NOT_GOING", label: "Not going", count: (c) => c.notGoing },
];
const ROLE_LABEL: Record<string, string> = { ORGANIZER: "Organizer", CO_ORGANIZER: "Co-organizer", INVITEE: "Invited", MEMBER: "Chat member" };

export default function EventDetail({ eventId }: { eventId: string }) {
  const router = useRouter();
  const zone = useMemo(() => viewerTimeZone(), []);
  const query = useEvent(eventId);
  const changed = useEventChanged();
  const [editing, setEditing] = useState(false);
  const [dialog, setDialog] = useState<"cancel" | "delete" | "share" | "invite" | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  if (query.isPending) return <div className="flex justify-center py-24" role="status" aria-label="Loading event"><Spinner /></div>;
  if (query.isError) {
    return (
      <div className="mx-auto max-w-xl px-4 py-16 text-center">
        <span className="mx-auto flex size-14 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><CalendarClock className="size-7" aria-hidden /></span>
        <h1 className="mt-4 text-xl font-semibold">{isNotFound(query.error) ? "This event isn't available" : "The event couldn't be loaded"}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{isNotFound(query.error) ? "It may have been deleted, or you're no longer one of its people." : eventErrorMessage(query.error)}</p>
        <div className="mt-5 flex justify-center gap-2">
          {!isNotFound(query.error) ? <Button variant="outline" size="sm" onClick={() => void query.refetch()}>Try again</Button> : null}
          <Button size="sm" asChild><Link href="/dashboard/events">All events</Link></Button>
        </div>
      </div>
    );
  }
  const event = query.data;
  return (
    <>
      <EventView event={event} zone={zone} status={status} setStatus={setStatus} onEdit={() => setEditing(true)} onDialog={setDialog} />
      <EventForm open={editing} event={event} onClose={() => setEditing(false)} onSaved={(saved) => { setEditing(false); changed(saved); setStatus("Event saved."); void query.refetch(); }} />
      {dialog === "cancel" ? <CancelDialog event={event} onClose={() => setDialog(null)} onDone={(saved) => { setDialog(null); changed(saved); setStatus("Event cancelled. Everyone on it is told."); }} /> : null}
      {dialog === "delete" ? <DeleteDialog event={event} onClose={() => setDialog(null)} onDone={() => { changed(null, event.eventId); router.push("/dashboard/events"); }} /> : null}
      {dialog === "share" ? <ShareDialog event={event} onClose={() => setDialog(null)} onDone={(title) => { setDialog(null); setStatus(`Shared in ${title}.`); }} /> : null}
      {dialog === "invite" ? <InviteDialog event={event} onClose={() => setDialog(null)} onDone={(n) => { setDialog(null); changed(null, event.eventId); void query.refetch(); setStatus(`${n} ${n === 1 ? "person" : "people"} invited.`); }} /> : null}
    </>
  );
}

function EventView({ event, zone, status, setStatus, onEdit, onDialog }: {
  event: CalendarEvent; zone: string; status: string | null; setStatus: (s: string | null) => void; onEdit: () => void; onDialog: (d: "cancel" | "delete" | "share" | "invite") => void;
}) {
  const { snapshot } = useMessagingSnapshot();
  const me = snapshot?.identity?.principalId ?? null;
  const cancelled = event.state === "CANCELLED";
  const [now] = useState(() => Date.now());
  const ended = !cancelled && !event.nextOccurrence && Date.parse(event.seriesEndsAt ?? event.endsAt) <= now;
  const occurrence = event.nextOccurrence ?? { startsAt: event.startsAt, endsAt: event.endsAt };
  const chat = event.conversationId ? snapshot?.conversations.find((c) => c.id === event.conversationId) ?? null : null;
  const organizers = [event.organizerPrincipalId, ...(event.coOrganizerPrincipalIds ?? [])];
  const attendees = useEventAttendees(event.eventId);
  const people = attendees.data?.attendees ?? [];
  const name = usePrincipalNames([...organizers, ...people.map((p) => p.principalId)], event.conversationId ? [event.conversationId] : []);
  const respond = useRespond(event.eventId);
  const changed = useEventChanged();
  const reminders = useMutation({
    mutationFn: (minutes: number[]) => eventsService.setReminders(event.eventId, minutes),
    onSuccess: (minutes) => { changed({ ...event, myReminders: minutes }); setStatus("Reminders saved."); },
  });
  const origin = typeof window === "undefined" ? "" : window.location.origin;
  const meeting = event.type === "ONLINE" ? meetingTarget(event, origin) : null;
  // A call: in the event's chat; a personal event: the direct chat with the organizer, if there is one.
  const direct = !event.conversationId && event.organizerPrincipalId !== me
    ? snapshot?.conversations.find((c) => c.contextClass !== "GROUP" && c.members.length === 2 && c.members.includes(event.organizerPrincipalId)) ?? null
    : null;
  const callConversation = event.conversationId ?? direct?.id ?? null;
  const [exporting, setExporting] = useState(false);
  const exportIcs = async () => {
    setExporting(true);
    try {
      saveCalendarFile(await eventsService.exportEvent(event.eventId), event.title);
      setStatus("Calendar file downloaded.");
    } catch (e) {
      setStatus(eventErrorMessage(e));
    } finally {
      setExporting(false);
    }
  };
  const answerable = !cancelled && !ended && event.myRole !== "ORGANIZER";
  const grouped = RSVP.map((r) => ({ ...r, people: people.filter((p) => p.response === r.value) }));
  const awaiting = people.filter((p) => !p.response && p.role !== "ORGANIZER");

  return (
    <div className="mx-auto w-full max-w-4xl px-4 pb-24 pt-4 sm:px-6 md:pb-10">
      <Link href="/dashboard/events" className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground"><ChevronLeft className="size-4" />Events</Link>
      <p aria-live="polite" role="status" className="sr-only">{status}</p>

      <div className="mt-3 overflow-hidden rounded-3xl border border-border bg-card">
        <EventCover mediaId={event.coverMediaId} className="h-40 w-full sm:h-56" alt={`Cover of ${event.title}`} />
        <div className="p-5 sm:p-7">
          {cancelled ? (
            <div role="alert" className="mb-4 flex gap-3 rounded-2xl border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
              <Ban className="mt-0.5 size-4 shrink-0" aria-hidden />
              <span><span className="font-semibold">This event was cancelled</span>{event.cancelledAt ? ` on ${new Date(event.cancelledAt).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" })}` : ""}.{event.cancelReason ? <span className="mt-0.5 block text-destructive/90">&ldquo;{event.cancelReason}&rdquo;</span> : null}</span>
            </div>
          ) : ended ? <p className="mb-4 rounded-2xl bg-muted px-3 py-2 text-sm text-muted-foreground">This event has ended.</p> : null}

          <div className="flex flex-wrap items-start gap-3">
            <h1 className={cn("min-w-0 flex-1 text-2xl font-bold tracking-tight", cancelled && "text-muted-foreground line-through")}>{event.title}</h1>
            <TypeBadge type={event.type} className="mt-1.5" />
          </div>

          <dl className="mt-4 space-y-3 text-sm">
            <div className="flex gap-3"><dt className="sr-only">When</dt><CalendarClock className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
              <dd><WhenLines startsAt={occurrence.startsAt} endsAt={occurrence.endsAt} event={event} viewerZone={zone} withYear />
                {event.nextOccurrence && event.recurrence ? <span className="block text-xs text-muted-foreground">Next occurrence</span> : null}
              </dd>
            </div>
            {event.recurrence ? (
              <div className="flex gap-3"><dt className="sr-only">Repeats</dt><Repeat className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                <dd>{recurrenceSummary(event.recurrence, localDate(event.startsAt, event.timeZone))}</dd>
              </div>
            ) : null}
            {event.location ? (
              <div className="flex gap-3"><dt className="sr-only">Location</dt><MapPin className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                <dd className="whitespace-pre-wrap break-words">{event.location}</dd>
              </div>
            ) : null}
            {event.conversationId ? (
              <div className="flex gap-3"><dt className="sr-only">Chat</dt><MessageSquare className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                <dd><Link className="font-medium text-primary hover:underline" href={`/dashboard/messages?conversation=${encodeURIComponent(event.conversationId)}`}>{chat?.title ?? "Open the chat"}</Link></dd>
              </div>
            ) : null}
            <div className="flex gap-3"><dt className="sr-only">Organizers</dt><Users className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
              <dd>Organized by <span className="font-medium">{name(event.organizerPrincipalId, "the organizer")}</span>
                {event.coOrganizerPrincipalIds?.length ? <> with {event.coOrganizerPrincipalIds.map((p) => name(p)).join(", ")}</> : null}
              </dd>
            </div>
          </dl>

          {event.description ? <p className="mt-5 whitespace-pre-wrap break-words text-sm leading-relaxed">{event.description}</p> : null}

          {/* Join, share, calendar */}
          <div className="mt-6 flex flex-wrap gap-2">
            {!cancelled && meeting ? (
              meeting.external
                ? <Button asChild><a href={meeting.href} target="_blank" rel="noopener noreferrer"><Video className="size-4" />Join meeting</a></Button>
                : <Button asChild><Link href={meeting.href}><Video className="size-4" />Join meeting</Link></Button>
            ) : null}
            {!cancelled && event.type === "CALL" && callConversation ? (
              <Button asChild><Link href={callHref({ conversationId: callConversation }, "VOICE", chat?.title ?? event.title)}><Phone className="size-4" />Join call</Link></Button>
            ) : null}
            {!cancelled && snapshot ? <Button variant="outline" onClick={() => onDialog("share")}><Send className="size-4" />Share in a chat</Button> : null}
            <Button variant="outline" onClick={() => void exportIcs()} disabled={exporting}>{exporting ? <Spinner size="sm" /> : <Download className="size-4" />}Add to calendar</Button>
          </div>
          {status ? <p className="mt-3 text-sm text-muted-foreground">{status}</p> : null}

          {/* Organizers */}
          {event.canEdit || event.canDelete ? (
            <div className="mt-4 flex flex-wrap gap-2 border-t border-border pt-4">
              {event.canEdit && !cancelled ? <Button variant="ghost" size="sm" onClick={onEdit}><Pencil className="size-4" />Edit</Button> : null}
              {event.canEdit && !cancelled ? <Button variant="ghost" size="sm" onClick={() => onDialog("invite")}><UserPlus className="size-4" />Invite people</Button> : null}
              {event.canEdit && !cancelled && !ended ? <Button variant="ghost" size="sm" className="text-destructive hover:bg-destructive/10" onClick={() => onDialog("cancel")}><Ban className="size-4" />Cancel event</Button> : null}
              {event.canDelete ? <Button variant="ghost" size="sm" className="text-destructive hover:bg-destructive/10" onClick={() => onDialog("delete")}><Trash2 className="size-4" />Delete</Button> : null}
            </div>
          ) : null}
        </div>
      </div>

      <div className="mt-5 grid gap-5 lg:grid-cols-[minmax(0,1fr)_320px]">
        <div className="space-y-5">
          {/* RSVP */}
          <section className="rounded-3xl border border-border bg-card p-5" aria-labelledby="rsvp-title">
            <h2 id="rsvp-title" className={sectionTitle}>{answerable ? "Are you going?" : "Answers"}</h2>
            <div role="group" aria-label="Your answer" className="mt-3 grid grid-cols-3 gap-2">
              {RSVP.map((r) => {
                const on = event.myResponse === r.value;
                return (
                  <button key={r.value} type="button" aria-pressed={on} disabled={!answerable || respond.isPending} onClick={() => respond.mutate(r.value)}
                    className={cn("flex flex-col items-center rounded-2xl border px-2 py-2.5 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-default",
                      on ? "border-primary bg-primary text-primary-foreground" : "border-border hover:bg-muted disabled:hover:bg-transparent")}>
                    {r.label}<span className={cn("text-xs tabular-nums", on ? "text-primary-foreground/80" : "text-muted-foreground")}>{r.count(event.counts)}</span>
                  </button>
                );
              })}
            </div>
            <p className="mt-2 text-xs text-muted-foreground">{event.counts.awaiting} not answered yet{event.recurrence ? " · your answer applies to every occurrence" : ""}</p>
            {respond.isError ? <p role="alert" className="mt-2 text-sm text-destructive">{eventErrorMessage(respond.error)}</p> : null}
          </section>

          {/* People */}
          <section className="rounded-3xl border border-border bg-card p-5" aria-labelledby="people-title">
            <h2 id="people-title" className={sectionTitle}>People</h2>
            {attendees.isPending ? <Spinner size="sm" className="mt-3" /> : attendees.isError ? <p role="alert" className="mt-3 text-sm text-destructive">{eventErrorMessage(attendees.error)}</p> : (
              <div className="mt-3 space-y-4">
                {grouped.filter((g) => g.people.length).map((g) => (
                  <PeopleGroup key={g.value} title={g.label} people={g.people} name={name} event={event} />
                ))}
                {awaiting.length ? <PeopleGroup title="Not answered" people={awaiting} name={name} event={event} /> : null}
                {!people.length ? <p className="text-sm text-muted-foreground">{event.scope === "CONVERSATION" ? "Everyone in the chat can answer." : "No one has been invited yet."}</p> : null}
              </div>
            )}
          </section>
        </div>

        <div className="space-y-5">
          {event.upcomingOccurrences?.length && event.recurrence ? (
            <section className="rounded-3xl border border-border bg-card p-5" aria-labelledby="next-title">
              <h2 id="next-title" className={sectionTitle}>Next occurrences</h2>
              <ul className="mt-3 space-y-2 text-sm">
                {event.upcomingOccurrences.map((o) => <li key={o.startsAt}><WhenLines startsAt={o.startsAt} endsAt={o.endsAt} event={event} viewerZone={zone} /></li>)}
              </ul>
            </section>
          ) : null}
          {!cancelled && !ended ? (
            <section className="rounded-3xl border border-border bg-card p-5" aria-labelledby="reminders-title">
              <h2 id="reminders-title" className={sectionTitle}>Your reminders</h2>
              <p className="mb-3 mt-1 text-xs text-muted-foreground">Only you get these.</p>
              <RemindersEditor value={event.myReminders ?? []} onChange={(v) => reminders.mutate(v)} disabled={reminders.isPending} />
              {reminders.isError ? <p role="alert" className="mt-2 text-sm text-destructive">{eventErrorMessage(reminders.error)}</p> : null}
            </section>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function PeopleGroup({ title, people, name, event }: { title: string; people: EventAttendee[]; name: (p: string) => string; event: CalendarEvent }) {
  const changed = useEventChanged();
  const remove = useMutation({
    mutationFn: (principalId: string) => eventsService.removeInvitee(event.eventId, principalId),
    onSuccess: () => changed(null, event.eventId),
  });
  return (
    <div>
      <p className="mb-1.5 text-sm font-medium">{title} <span className="font-normal text-muted-foreground">· {people.length}</span></p>
      <ul className="space-y-1">
        {people.map((p) => (
          <li key={p.principalId} className="flex items-center gap-3 rounded-xl px-1 py-1">
            <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xs font-semibold text-primary" aria-hidden>{name(p.principalId).slice(0, 1).toUpperCase()}</span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm">{name(p.principalId)}</span>
              <span className="block text-xs text-muted-foreground">{ROLE_LABEL[p.role] ?? p.role}</span>
            </span>
            {event.canEdit && p.role === "INVITEE" && event.state === "ACTIVE" ? (
              <button type="button" onClick={() => remove.mutate(p.principalId)} disabled={remove.isPending} aria-label={`Remove ${name(p.principalId)} from the event`}
                className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><X className="size-4" /></button>
            ) : null}
          </li>
        ))}
      </ul>
      {remove.isError ? <p role="alert" className="text-xs text-destructive">{eventErrorMessage(remove.error)}</p> : null}
    </div>
  );
}

function SmallDialog({ title, description, onClose, children }: { title: string; description?: string; onClose: () => void; children: React.ReactNode }) {
  return (
    <Dialog open onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader className="text-left">
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        {children}
      </DialogContent>
    </Dialog>
  );
}

function CancelDialog({ event, onClose, onDone }: { event: CalendarEvent; onClose: () => void; onDone: (e: CalendarEvent) => void }) {
  const [reason, setReason] = useState("");
  const cancel = useMutation({ mutationFn: () => eventsService.cancel(event.eventId, reason), onSuccess: onDone });
  return (
    <SmallDialog title="Cancel this event?" description={`Everyone on "${event.title}" is told it's cancelled${event.recurrence ? ", for every occurrence" : ""}. It stays visible as cancelled.`} onClose={onClose}>
      <label className="grid gap-1.5 text-sm font-medium">Reason <span className="-mt-1 text-xs font-normal text-muted-foreground">Optional, shown to everyone</span>
        <textarea value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} rows={3} className="rounded-xl border border-transparent bg-muted/70 px-3 py-2 text-sm focus:border-primary focus:bg-background focus:outline-none focus:ring-2 focus:ring-primary/20" />
      </label>
      {cancel.isError ? <p role="alert" className="text-sm text-destructive">{eventErrorMessage(cancel.error)}</p> : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" onClick={onClose}>Keep it</Button>
        <Button variant="destructive" onClick={() => cancel.mutate()} disabled={cancel.isPending}>{cancel.isPending ? <Spinner size="sm" /> : null}Cancel event</Button>
      </div>
    </SmallDialog>
  );
}

function DeleteDialog({ event, onClose, onDone }: { event: CalendarEvent; onClose: () => void; onDone: () => void }) {
  const remove = useMutation({ mutationFn: () => eventsService.remove(event.eventId), onSuccess: onDone });
  return (
    <SmallDialog title="Delete this event?" description={`"${event.title}" is removed for everyone, and no one is told. To let people know, cancel it first.`} onClose={onClose}>
      {remove.isError ? <p role="alert" className="text-sm text-destructive">{eventErrorMessage(remove.error)}</p> : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" onClick={onClose}>Keep it</Button>
        <Button variant="destructive" onClick={() => remove.mutate()} disabled={remove.isPending}>{remove.isPending ? <Spinner size="sm" /> : null}Delete</Button>
      </div>
    </SmallDialog>
  );
}

/** Post the event's link in one of the user's chats (the normal send path; the chat shows it as an event card). */
function ShareDialog({ event, onClose, onDone }: { event: CalendarEvent; onClose: () => void; onDone: (title: string) => void }) {
  const { engine, snapshot } = useMessagingSnapshot();
  const [search, setSearch] = useState("");
  const needle = search.trim().toLocaleLowerCase();
  const chats = (snapshot?.conversations ?? []).filter((c) => c.title && (!needle || c.title.toLocaleLowerCase().includes(needle))).slice(0, 100);
  const send = useMutation({
    mutationFn: async (conversationId: string) => {
      if (!engine) throw new Error("Chats aren't available here.");
      await engine.send(conversationId, eventShareText(window.location.origin, event));
      return snapshot?.conversations.find((c) => c.id === conversationId)?.title ?? "the chat";
    },
    onSuccess: onDone,
  });
  return (
    <SmallDialog title="Share in a chat" description="Sends a message with the event link. Only people who can see the event can open it." onClose={onClose}>
      <input type="search" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="Search chats" aria-label="Search chats" className="h-10 w-full rounded-xl border border-transparent bg-muted/70 px-3 text-sm focus:border-primary focus:bg-background focus:outline-none focus:ring-2 focus:ring-primary/20" />
      <ul className="no-scrollbar max-h-72 space-y-1 overflow-y-auto">
        {chats.map((c) => (
          <li key={c.id}>
            <button type="button" onClick={() => send.mutate(c.id)} disabled={send.isPending} className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left text-sm hover:bg-muted disabled:opacity-60">
              <span className="flex size-9 items-center justify-center rounded-full bg-primary/10 text-primary" aria-hidden>{c.contextClass === "GROUP" ? <Users className="size-4" /> : c.title.slice(0, 1).toUpperCase()}</span>
              <span className="min-w-0 flex-1 truncate">{c.title}</span>
              {send.isPending && send.variables === c.id ? <Spinner size="sm" /> : <Send className="size-4 text-muted-foreground" aria-hidden />}
            </button>
          </li>
        ))}
        {!chats.length ? <li className="px-2 py-3 text-sm text-muted-foreground">{needle ? "No chat matches." : "No chats yet."}</li> : null}
      </ul>
      {send.isError ? <p role="alert" className="text-sm text-destructive">{eventErrorMessage(send.error)}</p> : null}
    </SmallDialog>
  );
}

/** Invite more people: mutual contacts (personal) or members of the event's chat. */
function InviteDialog({ event, onClose, onDone }: { event: CalendarEvent; onClose: () => void; onDone: (n: number) => void }) {
  const { snapshot } = useMessagingSnapshot();
  const me = snapshot?.identity?.principalId ?? null;
  const attendees = useEventAttendees(event.eventId);
  const already = new Set([event.organizerPrincipalId, ...(event.coOrganizerPrincipalIds ?? []), ...(attendees.data?.attendees ?? []).filter((a) => a.role !== "MEMBER").map((a) => a.principalId)]);
  const [selected, setSelected] = useState<string[]>([]);
  const personal = event.scope !== "CONVERSATION";
  const contacts = useQuery({ queryKey: ["v2", "messaging", "events", "eligible-contacts"], queryFn: eventsService.eligibleContacts, enabled: personal, staleTime: 60_000, retry: false });
  const memberIds = useMemo(() => (snapshot?.conversations.find((c) => c.id === event.conversationId)?.members ?? []).filter((p) => p !== me), [snapshot?.conversations, event.conversationId, me]);
  const name = usePrincipalNames(memberIds, event.conversationId ? [event.conversationId] : []);
  const people = (personal ? contacts.data ?? [] : memberIds.map((p) => ({ principalId: p, name: name(p, "Member") }))).filter((p) => !already.has(p.principalId));
  const invite = useMutation({ mutationFn: () => eventsService.invite(event.eventId, selected), onSuccess: () => onDone(selected.length) });
  return (
    <SmallDialog title="Invite people" description={personal ? "Only your mutual contacts can be invited." : "Invited members are asked directly; everyone in the chat can already answer."} onClose={onClose}>
      <PeoplePicker label={personal ? "Contacts" : "Members"} people={people} selected={selected} onChange={setSelected} max={LIMITS.invitees}
        loading={personal ? contacts.isPending : false} error={personal && contacts.isError ? eventErrorMessage(contacts.error) : null} />
      {invite.isError ? <p role="alert" className="text-sm text-destructive">{eventErrorMessage(invite.error)}</p> : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" onClick={onClose}>Close</Button>
        <Button onClick={() => invite.mutate()} disabled={!selected.length || invite.isPending}>{invite.isPending ? <Spinner size="sm" /> : <CalendarPlus className="size-4" />}Invite{selected.length ? ` ${selected.length}` : ""}</Button>
      </div>
    </SmallDialog>
  );
}
