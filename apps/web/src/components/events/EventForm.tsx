"use client";

/**
 * Create / edit an event (ADR 0071). Personal events invite mutual contacts; chat events
 * belong to one conversation and invite its members. Online events only LINK to a CF Meet
 * meeting: one of the user's upcoming meetings, or a new one made through the existing
 * Meeting API (there is no second scheduler). Edits send only what changed with the
 * `version` read; a stale version (409) offers the latest copy.
 */
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Bell, Check, ImagePlus, Link2, Plus, RefreshCw, Search, Trash2, Video, X } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { NiceSelect } from "@/components/ui/nice-select";
import { Spinner } from "@/components/ui/spinner";
import { TimeZoneField } from "@/components/meetings/ScheduleFields";
import { cn } from "@/lib/utils";
import { viewerTimeZone, validTimeZone } from "@/lib/meeting-time";
import { conferencesService } from "@/services/v2/conferences.service";
import { spacesService } from "@/services/v2/spaces.service";
import { useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import { COVER_MAX_BYTES, eventErrorMessage, eventsService, isVersionConflict } from "@/services/v2/events.service";
import {
  EVENT_TYPES,
  LIMITS,
  REMINDER_CHOICES,
  WEEKDAYS,
  WEEKDAY_SHORT,
  buildCreateBody,
  buildPatchBody,
  emptyForm,
  eventShareText,
  eventTimes,
  formFromEvent,
  meetingJoinUrl,
  reminderLabel,
  validateForm,
  weekdayOf,
  type CalendarEvent,
  type EventFormState,
  type RecurrenceChoice,
  type Weekday,
} from "@/lib/events/core";
import { EventCover, TYPE_ICON, field, sectionTitle, useMessagingSnapshot } from "./event-ui";

type Person = { principalId: string; name: string };

export default function EventForm({ open, onClose, event, conversationId, onSaved }: {
  open: boolean;
  onClose: () => void;
  /** Edit this event (PATCH); omitted: create one. */
  event?: CalendarEvent | null;
  /** Create the event in this chat. */
  conversationId?: string | null;
  onSaved: (event: CalendarEvent, created: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="flex max-h-[92dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-2xl">
        <DialogHeader className="border-b border-border px-5 py-4 text-left">
          <DialogTitle>{event ? "Edit event" : "Create event"}</DialogTitle>
          <DialogDescription>{event ? "Changes apply to every occurrence and everyone on the event sees them." : "Plan something with your contacts or a chat."}</DialogDescription>
        </DialogHeader>
        {open ? <FormBody key={event ? `${event.eventId}:${event.version}` : `new:${conversationId ?? ""}`} event={event ?? null} conversationId={conversationId ?? null} onClose={onClose} onSaved={onSaved} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function FormBody({ event: initial, conversationId, onClose, onSaved }: { event: CalendarEvent | null; conversationId: string | null; onClose: () => void; onSaved: (event: CalendarEvent, created: boolean) => void }) {
  const [event, setEvent] = useState(initial);
  const editing = !!event;
  const [state, setState] = useState<EventFormState>(() => (initial ? formFromEvent(initial) : emptyForm(viewerTimeZone(), Date.now(), conversationId)));
  const set = <K extends keyof EventFormState>(key: K, value: EventFormState[K]) => setState((s) => ({ ...s, [key]: value }));
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [busy, setBusy] = useState(false);
  const key = useRef<{ sig: string; key: string } | null>(null);
  const { engine, snapshot } = useMessagingSnapshot();
  const me = snapshot?.identity?.principalId ?? null;
  const ids = useId();
  const organizer = !event || event.myRole === "ORGANIZER";

  // The chats an event can belong to, and the people who can be added.
  const chats = useMemo(() => (snapshot?.conversations ?? []).filter((c) => c.title).map((c) => ({ value: c.id, label: c.title })), [snapshot?.conversations]);
  const chat = snapshot?.conversations.find((c) => c.id === state.conversationId) ?? null;
  const contacts = useQuery({ queryKey: ["v2", "messaging", "events", "eligible-contacts"], queryFn: eventsService.eligibleContacts, enabled: state.scope === "PERSONAL", staleTime: 60_000, retry: false });
  const memberIds = useMemo(() => (chat?.members ?? []).filter((p) => p !== me), [chat, me]);
  const memberNames = useQuery({
    queryKey: ["v2", "messaging", "events", "names", "members", state.conversationId, memberIds],
    queryFn: () => eventsService.names(memberIds.filter((p) => !snapshot?.names[p]), [state.conversationId]),
    enabled: state.scope === "CONVERSATION" && memberIds.length > 0,
    staleTime: 5 * 60_000,
  });
  const people: Person[] = useMemo(() => state.scope === "PERSONAL"
    ? (contacts.data ?? [])
    : memberIds.map((p) => ({ principalId: p, name: snapshot?.names[p] || memberNames.data?.get(p) || "Member" })), [state.scope, contacts.data, memberIds, snapshot?.names, memberNames.data]);
  // Co-organizers already on an event stay listed (and removable) even when not in the list now.
  const coChoices = useMemo(() => {
    const extra = state.coOrganizerPrincipalIds.filter((p) => !people.some((x) => x.principalId === p)).map((p) => ({ principalId: p, name: snapshot?.names[p] || "Co-organizer" }));
    return [...people, ...extra];
  }, [people, state.coOrganizerPrincipalIds, snapshot?.names]);


  const reloadLatest = async () => {
    if (!event) return;
    setBusy(true);
    try {
      const latest = await eventsService.get(event.eventId);
      setEvent(latest);
      setState(formFromEvent(latest));
      setConflict(false);
      setError(null);
    } catch (e) {
      setError(eventErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    setError(null);
    setConflict(false);
    const problem = validateForm(state);
    if (problem) { setError(problem); return; }
    setBusy(true);
    try {
      if (event) {
        const patch = buildPatchBody(state, event);
        if (!organizer) delete patch.coOrganizerPrincipalIds;
        let saved = Object.keys(patch).length > 1 ? await eventsService.update(event.eventId, patch) : event;
        const wanted = [...new Set(state.reminders)].sort((a, b) => a - b);
        if (JSON.stringify(wanted) !== JSON.stringify([...(event.myReminders ?? [])].sort((a, b) => a - b))) {
          saved = { ...saved, myReminders: await eventsService.setReminders(event.eventId, wanted) };
        }
        onSaved(saved, false);
      } else {
        const body = buildCreateBody(state);
        // One key per user action: a retry of the same form returns the same event.
        const sig = JSON.stringify(body);
        if (key.current?.sig !== sig) key.current = { sig, key: crypto.randomUUID() };
        const created = await eventsService.create(body, key.current.key);
        if (body.scope === "CONVERSATION" && body.conversationId && engine) {
          // The chat gets a message with the event link (rendered as an event card).
          try { await engine.send(body.conversationId, eventShareText(window.location.origin, created)); } catch { /* the event exists; it can be shared from its page */ }
        }
        onSaved(created, true);
      }
    } catch (e) {
      if (isVersionConflict(e)) setConflict(true);
      setError(eventErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="flex min-h-0 flex-1 flex-col" onSubmit={(e) => { e.preventDefault(); void submit(); }} noValidate>
      <fieldset disabled={busy} className="no-scrollbar min-h-0 flex-1 space-y-6 overflow-y-auto px-5 py-5">
        <CoverField mediaId={state.coverMediaId} onChange={(id) => set("coverMediaId", id)} />

        <div className="grid gap-3">
          <label className="grid gap-1.5 text-sm font-medium">Title
            <input className={field} value={state.title} maxLength={LIMITS.title} required onChange={(e) => set("title", e.target.value)} placeholder="e.g. Birthday dinner" />
          </label>
          <label className="grid gap-1.5 text-sm font-medium">Description <span className="-mt-1 text-xs font-normal text-muted-foreground">Optional</span>
            <textarea className={cn(field, "h-auto min-h-[84px] py-2.5")} rows={3} maxLength={LIMITS.description} value={state.description} onChange={(e) => set("description", e.target.value)} placeholder="What to expect, what to bring" />
          </label>
        </div>

        {/* Kind of event */}
        <section className="space-y-3">
          <p className={sectionTitle} id={`${ids}-type`}>Where</p>
          <div role="radiogroup" aria-labelledby={`${ids}-type`} className="grid grid-cols-3 gap-2">
            {EVENT_TYPES.map((t) => {
              const Icon = TYPE_ICON[t.value];
              const on = state.type === t.value;
              return (
                <button key={t.value} type="button" role="radio" aria-checked={on} onClick={() => set("type", t.value)}
                  className={cn("flex h-11 items-center justify-center gap-2 rounded-xl border text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", on ? "border-primary bg-primary/10 text-primary" : "border-border text-muted-foreground hover:bg-muted")}>
                  <Icon className="size-4" aria-hidden />{t.label}
                </button>
              );
            })}
          </div>
          {state.type === "ONLINE" ? <MeetingPicker state={state} setState={setState} /> : null}
          {state.type === "CALL" ? <p className="text-xs text-muted-foreground">People join with a call {state.scope === "CONVERSATION" ? "in the chat" : "with the organizer"} from the event page.</p> : null}
          <label className="grid gap-1.5 text-sm font-medium">Location <span className="-mt-1 text-xs font-normal text-muted-foreground">{state.type === "PHYSICAL" ? "Address or place" : "Optional"}</span>
            <input className={field} value={state.location} maxLength={LIMITS.location} onChange={(e) => set("location", e.target.value)} placeholder={state.type === "PHYSICAL" ? "e.g. 12 Marina Road, Lagos" : ""} />
          </label>
        </section>

        {/* When */}
        <section className="space-y-3">
          <div className="flex items-center justify-between">
            <p className={sectionTitle}>When</p>
            <label className="flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
              All day
              <button type="button" role="switch" aria-checked={state.allDay} aria-label="All day" onClick={() => set("allDay", !state.allDay)}
                className={cn("relative h-6 w-11 rounded-full transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", state.allDay ? "bg-primary" : "bg-muted-foreground/30")}>
                <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", state.allDay ? "left-[22px]" : "left-0.5")} />
              </button>
            </label>
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="grid grid-cols-[1fr_auto] gap-2">
              <label className="grid gap-1.5 text-sm font-medium">Starts
                <input type="date" className={field} value={state.startDate} onChange={(e) => {
                  const v = e.target.value;
                  setState((s) => withStartDay(s.endDate < v || s.endDate === s.startDate ? { ...s, startDate: v, endDate: v } : { ...s, startDate: v }));
                }} required />
              </label>
              {!state.allDay ? <label className="grid gap-1.5 text-sm font-medium"><span className="sr-only">Start time</span><span aria-hidden className="invisible">.</span>
                <input type="time" aria-label="Start time" className={cn(field, "w-32")} value={state.startTime} onChange={(e) => set("startTime", e.target.value)} required />
              </label> : null}
            </div>
            <div className="grid grid-cols-[1fr_auto] gap-2">
              <label className="grid gap-1.5 text-sm font-medium">Ends
                <input type="date" className={field} value={state.endDate} min={state.startDate} onChange={(e) => set("endDate", e.target.value)} required />
              </label>
              {!state.allDay ? <label className="grid gap-1.5 text-sm font-medium"><span className="sr-only">End time</span><span aria-hidden className="invisible">.</span>
                <input type="time" aria-label="End time" className={cn(field, "w-32")} value={state.endTime} onChange={(e) => set("endTime", e.target.value)} required />
              </label> : null}
            </div>
          </div>
          <TimeZoneField id={`${ids}-tz`} label="Time zone" value={state.timeZone} onChange={(zone) => set("timeZone", validTimeZone(zone) ? zone : "UTC")} />
          <RecurrenceFields state={state} setState={setState} />
        </section>

        {/* Who */}
        <section className="space-y-3">
          <p className={sectionTitle}>Who</p>
          {editing ? (
            <p className="text-sm text-muted-foreground">{state.scope === "CONVERSATION" ? `Chat event${chat ? ` in ${chat.title}` : ""}` : "Personal event"}. Invite more people from the event page.</p>
          ) : (
            <>
              <div role="radiogroup" aria-label="Who the event is for" className="grid grid-cols-2 gap-2">
                {([["PERSONAL", "Personal", "Invite your contacts"], ["CONVERSATION", "A chat", "Everyone in the chat"]] as const).map(([value, label, hint]) => (
                  <button key={value} type="button" role="radio" aria-checked={state.scope === value}
                    onClick={() => setState((s) => ({ ...s, scope: value, coOrganizerPrincipalIds: [], inviteePrincipalIds: [] }))}
                    className={cn("rounded-xl border px-3 py-2.5 text-left text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", state.scope === value ? "border-primary bg-primary/10" : "border-border hover:bg-muted")}>
                    <span className="block font-medium">{label}</span><span className="block text-xs text-muted-foreground">{hint}</span>
                  </button>
                ))}
              </div>
              {state.scope === "CONVERSATION" ? (
                chats.length ? <NiceSelect label="Chat" value={state.conversationId} placeholder="Choose a chat" options={chats}
                  onChange={(v) => setState((s) => ({ ...s, conversationId: v, coOrganizerPrincipalIds: [], inviteePrincipalIds: [] }))} />
                  : <p className="text-sm text-muted-foreground">Your chats are still loading, or you have none yet.</p>
              ) : null}
            </>
          )}
          {organizer && (state.scope === "PERSONAL" || state.conversationId) ? (
            <PeoplePicker label="Co-organizers" hint={`They can edit, invite and cancel. Up to ${LIMITS.coOrganizers}.`} people={coChoices} selected={state.coOrganizerPrincipalIds}
              max={LIMITS.coOrganizers} loading={state.scope === "PERSONAL" ? contacts.isPending : false} error={state.scope === "PERSONAL" && contacts.isError ? eventErrorMessage(contacts.error) : null}
              onChange={(v) => setState((s) => ({ ...s, coOrganizerPrincipalIds: v, inviteePrincipalIds: s.inviteePrincipalIds.filter((p) => !v.includes(p)) }))} />
          ) : null}
          {!editing && (state.scope === "PERSONAL" || state.conversationId) ? (
            <PeoplePicker label={state.scope === "PERSONAL" ? "Invite contacts" : "Invite members"}
              hint={state.scope === "PERSONAL" ? "Only your mutual contacts can be invited." : "Everyone in the chat can see the event and answer; invited members are asked directly."}
              people={people.filter((p) => !state.coOrganizerPrincipalIds.includes(p.principalId))} selected={state.inviteePrincipalIds} max={LIMITS.invitees}
              loading={state.scope === "PERSONAL" ? contacts.isPending : false} error={state.scope === "PERSONAL" && contacts.isError ? eventErrorMessage(contacts.error) : null}
              onChange={(v) => set("inviteePrincipalIds", v)} />
          ) : null}
        </section>

        <section className="space-y-2">
          <p className={sectionTitle}>Your reminders</p>
          <RemindersEditor value={state.reminders} onChange={(v) => set("reminders", v)} />
        </section>
      </fieldset>

      <div className="border-t border-border px-5 py-3">
        <div aria-live="polite">
          {error ? (
            <div role="alert" className="mb-3 flex flex-wrap items-center gap-2 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">
              <span className="flex-1">{error}</span>
              {conflict ? <Button type="button" size="sm" variant="outline" onClick={() => void reloadLatest()}><RefreshCw className="size-4" />Load the latest</Button> : null}
            </div>
          ) : null}
        </div>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose} disabled={busy}>Cancel</Button>
          <Button type="submit" disabled={busy}>{busy ? <Spinner size="sm" /> : null}{editing ? "Save changes" : "Create event"}</Button>
        </div>
      </div>
    </form>
  );
}

// ---------------------------------------------------------------------------- cover

function CoverField({ mediaId, onChange }: { mediaId: string | null; onChange: (id: string | null) => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  const pick = async (file: File | undefined) => {
    if (!file) return;
    setError(null);
    if (!file.type.startsWith("image/")) { setError("Choose a photo for the cover."); return; }
    if (file.size > COVER_MAX_BYTES) { setError("The cover photo can be at most 8 MB."); return; }
    const url = URL.createObjectURL(file);
    setPreview(url);
    abort.current = new AbortController();
    setProgress(0);
    try {
      const media = await eventsService.uploadCover(file, setProgress, abort.current.signal);
      onChange(media.mediaId);
    } catch (e) {
      if ((e as Error).name !== "AbortError") setError(eventErrorMessage(e, "The photo didn't upload. Try again."));
      setPreview(null);
    } finally {
      setProgress(null);
    }
  };

  return (
    <div>
      <div className="relative h-36 overflow-hidden rounded-2xl border border-border sm:h-44">
        {preview ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={preview} alt="Cover preview" className="size-full object-cover" />
        ) : <EventCover mediaId={mediaId} className="size-full" alt="Event cover" />}
        <div className="absolute bottom-2 right-2 flex gap-2">
          <Button type="button" size="sm" variant="secondary" onClick={() => input.current?.click()} disabled={progress !== null}><ImagePlus className="size-4" />{mediaId || preview ? "Change cover" : "Add cover"}</Button>
          {mediaId || preview ? <Button type="button" size="sm" variant="secondary" aria-label="Remove cover" onClick={() => { abort.current?.abort(); setPreview(null); onChange(null); }}><Trash2 className="size-4" /></Button> : null}
        </div>
        {progress !== null ? (
          <div className="absolute inset-x-0 bottom-0 h-1 bg-black/20" role="progressbar" aria-label="Uploading cover" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress * 100)}>
            <div className="h-full bg-primary transition-[width]" style={{ width: `${Math.round(progress * 100)}%` }} />
          </div>
        ) : null}
      </div>
      <input ref={input} type="file" accept="image/*" className="hidden" aria-label="Cover photo" onChange={(e) => { void pick(e.target.files?.[0]); e.target.value = ""; }} />
      {error ? <p role="alert" className="mt-1.5 text-xs text-destructive">{error}</p> : null}
    </div>
  );
}

// ---------------------------------------------------------------------------- meeting

/** Link one of the user's upcoming CF Meet meetings, or create one with the Meeting API. */
function MeetingPicker({ state, setState }: { state: EventFormState; setState: React.Dispatch<React.SetStateAction<EventFormState>> }) {
  const caps = useMeetingCapabilities();
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<{ id: string; title: string; code: string } | null>(null);
  const spaceKey = useRef<string | null>(null);
  const [now] = useState(() => Date.now());
  const meetings = useQuery({
    queryKey: ["v2", "messaging", "events", "meeting-choices", caps.organizationId],
    queryFn: () => conferencesService.agenda({
      from: new Date(now - 3_600_000).toISOString(),
      to: new Date(now + 90 * 86_400_000).toISOString(),
      status: "CREATED,PREPARING,WAITING,LIVE",
      limit: 100,
      ...(caps.organizationId ? { organizationId: caps.organizationId } : {}),
    }),
    staleTime: 60_000,
    retry: false,
  });
  // One choice per meeting (its next occurrence).
  const choices = useMemo(() => {
    const seen = new Map<string, { id: string; title: string; code: string; when: string }>();
    for (const item of meetings.data?.items ?? []) {
      if (seen.has(item.meetingSpaceId)) continue;
      seen.set(item.meetingSpaceId, {
        id: item.meetingSpaceId,
        title: item.meetingSpace.title || "CF Meet meeting",
        code: item.meetingSpace.meetingCode,
        when: new Intl.DateTimeFormat(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" }).format(new Date(item.scheduledStartAt)),
      });
    }
    if (created && !seen.has(created.id)) seen.set(created.id, { ...created, when: "New" });
    return [...seen.values()];
  }, [meetings.data, created]);
  const origin = typeof window === "undefined" ? "" : window.location.origin;
  const choose = (id: string) => {
    const m = choices.find((c) => c.id === id);
    setState((s) => ({ ...s, meetingId: id, meetingUrl: m ? meetingJoinUrl(origin, m.code) ?? "" : "" }));
  };

  const create = async () => {
    setError(null);
    setCreating(true);
    try {
      const zone = validTimeZone(state.timeZone) ? state.timeZone : "UTC";
      if (!spaceKey.current) spaceKey.current = crypto.randomUUID();
      const organization = !caps.canCreatePersonal && caps.canCreateOrganization && caps.organizationId ? { organizationId: caps.organizationId } : {};
      const space = await spacesService.create({ title: state.title.trim() || "Event meeting", description: state.description.trim() || undefined, timezone: zone, ...organization }, spaceKey.current);
      spaceKey.current = null;
      // Put the event's time on the meeting's calendar too (a timed event of up to 24 hours in the future).
      try {
        const { startsAt, endsAt } = eventTimes(state);
        if (!state.allDay && Date.parse(startsAt) > Date.now() && Date.parse(endsAt) - Date.parse(startsAt) <= 86_400_000) {
          await conferencesService.create(space.id, { scheduledStartAt: startsAt, scheduledEndAt: endsAt }, crypto.randomUUID());
        }
      } catch { /* the meeting exists; its date can be set from the meeting page */ }
      setCreated({ id: space.id, title: space.title || "CF Meet meeting", code: space.meetingCode });
      setState((s) => ({ ...s, meetingId: space.id, meetingUrl: meetingJoinUrl(origin, space.meetingCode) ?? "" }));
    } catch (e) {
      setError(eventErrorMessage(e, "The meeting couldn't be created. Try again."));
    } finally {
      setCreating(false);
    }
  };

  const selected = choices.find((c) => c.id === state.meetingId);
  return (
    <div className="space-y-2 rounded-2xl border border-border p-3">
      <p className="flex items-center gap-2 text-sm font-medium"><Video className="size-4 text-primary" aria-hidden />CF Meet meeting</p>
      {meetings.isPending ? <Spinner size="sm" /> : (
        <NiceSelect value={selected ? state.meetingId : ""} placeholder={choices.length ? "Pick one of your meetings" : "No upcoming meetings"}
          options={choices.map((c) => ({ value: c.id, label: c.title, hint: `${c.when} · ${c.code}` }))} onChange={choose} disabled={!choices.length} />
      )}
      {state.meetingId && !selected ? <p className="flex items-center gap-1.5 text-xs text-muted-foreground"><Link2 className="size-3.5" />Linked meeting {state.meetingUrl || state.meetingId}</p> : null}
      <div className="flex flex-wrap items-center gap-2">
        {caps.canCreatePersonal || caps.canCreateOrganization ? (
          <Button type="button" size="sm" variant="outline" onClick={() => void create()} disabled={creating}>{creating ? <Spinner size="sm" /> : <Plus className="size-4" />}Create a new meeting</Button>
        ) : null}
        {state.meetingId || state.meetingUrl ? <Button type="button" size="sm" variant="ghost" onClick={() => setState((s) => ({ ...s, meetingId: "", meetingUrl: "" }))}><X className="size-4" />Unlink</Button> : null}
      </div>
      {meetings.isError ? <p className="text-xs text-muted-foreground">Your meetings couldn&apos;t be loaded. You can still create a new one.</p> : null}
      {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
    </div>
  );
}

// ---------------------------------------------------------------------------- recurrence

/** A weekly series' first day must be one of its days: keep the start day selected. */
function withStartDay(s: EventFormState): EventFormState {
  if (s.frequency !== "WEEKLY" || !/^\d{4}-\d{2}-\d{2}$/.test(s.startDate)) return s;
  const day = weekdayOf(s.startDate);
  return s.byDay.includes(day) ? s : { ...s, byDay: [...s.byDay, day] };
}

const FREQUENCIES: { value: RecurrenceChoice; label: string }[] = [
  { value: "NONE", label: "Does not repeat" },
  { value: "DAILY", label: "Daily" },
  { value: "WEEKLY", label: "Weekly" },
  { value: "MONTHLY", label: "Monthly" },
];

function RecurrenceFields({ state, setState }: { state: EventFormState; setState: React.Dispatch<React.SetStateAction<EventFormState>> }) {
  const id = useId();
  const unit = state.frequency === "DAILY" ? "day" : state.frequency === "WEEKLY" ? "week" : "month";
  const toggle = (d: Weekday) => setState((s) => ({ ...s, byDay: s.byDay.includes(d) ? s.byDay.filter((x) => x !== d) : [...s.byDay, d] }));
  const startDay = /^\d{4}-\d{2}-\d{2}$/.test(state.startDate) ? weekdayOf(state.startDate) : null;
  return (
    <div className="space-y-3">
      <NiceSelect label="Repeat" value={state.frequency} options={FREQUENCIES} onChange={(v) => setState((s) => withStartDay({ ...s, frequency: v }))} />
      {state.frequency !== "NONE" ? (
        <div className="space-y-3 rounded-2xl bg-muted/40 p-3">
          <label className="flex items-center gap-2 text-sm">Every
            <input type="number" min={1} max={LIMITS.interval} className={cn(field, "h-9 w-20 bg-background")} value={state.interval}
              onChange={(e) => setState((s) => ({ ...s, interval: Math.max(1, Math.min(LIMITS.interval, Number(e.target.value) || 1)) }))} />
            {state.interval === 1 ? unit : `${unit}s`}
          </label>
          {state.frequency === "WEEKLY" ? (
            <div role="group" aria-label="Days of the week" className="flex flex-wrap gap-1.5">
              {WEEKDAYS.map((d) => {
                const on = state.byDay.includes(d);
                const locked = d === startDay;
                return (
                  <button key={d} type="button" aria-pressed={on} disabled={locked && on} onClick={() => toggle(d)} title={locked ? "The first day of the series" : undefined}
                    className={cn("h-9 min-w-11 rounded-full px-3 text-sm font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40", on ? "bg-primary text-primary-foreground" : "bg-background text-muted-foreground hover:bg-muted", locked && on && "disabled:opacity-100")}>
                    {WEEKDAY_SHORT[d]}
                  </button>
                );
              })}
            </div>
          ) : null}
          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">Ends</legend>
            <label className="flex items-center gap-2 text-sm"><input type="radio" name={`${id}-ends`} checked={state.ends === "NEVER"} onChange={() => setState((s) => ({ ...s, ends: "NEVER" }))} />Never</label>
            <label className="flex flex-wrap items-center gap-2 text-sm"><input type="radio" name={`${id}-ends`} checked={state.ends === "COUNT"} onChange={() => setState((s) => ({ ...s, ends: "COUNT" }))} />After
              <input type="number" min={1} max={LIMITS.count} aria-label="Number of occurrences" className={cn(field, "h-9 w-24 bg-background")} value={state.count} disabled={state.ends !== "COUNT"}
                onChange={(e) => setState((s) => ({ ...s, count: Math.max(1, Math.min(LIMITS.count, Number(e.target.value) || 1)) }))} />times
            </label>
            <label className="flex flex-wrap items-center gap-2 text-sm"><input type="radio" name={`${id}-ends`} checked={state.ends === "UNTIL"} onChange={() => setState((s) => ({ ...s, ends: "UNTIL" }))} />On
              <input type="date" aria-label="Last date" className={cn(field, "h-9 w-44 bg-background")} value={state.until} min={state.startDate} disabled={state.ends !== "UNTIL"} onChange={(e) => setState((s) => ({ ...s, until: e.target.value }))} />
            </label>
          </fieldset>
        </div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------- people

export function PeoplePicker({ label, hint, people, selected, onChange, max, loading, error }: {
  label: string; hint?: string; people: Person[]; selected: string[]; onChange: (ids: string[]) => void; max: number; loading?: boolean; error?: string | null;
}) {
  const [search, setSearch] = useState("");
  const id = useId();
  const needle = search.trim().toLocaleLowerCase();
  const shown = people.filter((p) => !needle || p.name.toLocaleLowerCase().includes(needle)).slice(0, 200);
  const name = (p: string) => people.find((x) => x.principalId === p)?.name ?? "Contact";
  const toggle = (p: string) => onChange(selected.includes(p) ? selected.filter((x) => x !== p) : selected.length >= max ? selected : [...selected, p]);
  return (
    <div className="space-y-2">
      <p className="text-sm font-medium" id={`${id}-label`}>{label}{selected.length ? <span className="font-normal text-muted-foreground"> · {selected.length} selected</span> : null}</p>
      {hint ? <p className="-mt-1 text-xs text-muted-foreground">{hint}</p> : null}
      {selected.length ? (
        <ul className="flex flex-wrap gap-1.5" aria-label={`${label} selected`}>
          {selected.map((p) => (
            <li key={p}><button type="button" onClick={() => toggle(p)} aria-label={`Remove ${name(p)}`} className="inline-flex items-center gap-1 rounded-full bg-primary/10 px-2.5 py-1 text-xs font-medium text-primary hover:bg-primary/20">{name(p)}<X className="size-3" /></button></li>
          ))}
        </ul>
      ) : null}
      {loading ? <Spinner size="sm" /> : error ? <p role="alert" className="text-xs text-destructive">{error}</p> : people.length === 0 ? <p className="text-xs text-muted-foreground">No one to add yet.</p> : (
        <div className="rounded-xl border border-border">
          <div className="relative border-b border-border">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
            <input type="search" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="Search" aria-label={`Search ${label.toLowerCase()}`} className="h-10 w-full rounded-t-xl bg-transparent pl-9 pr-3 text-sm focus:outline-none" />
          </div>
          <ul role="group" aria-labelledby={`${id}-label`} className="no-scrollbar max-h-48 overflow-y-auto p-1">
            {shown.map((p) => {
              const on = selected.includes(p.principalId);
              return (
                <li key={p.principalId}>
                  <button type="button" role="checkbox" aria-checked={on} onClick={() => toggle(p.principalId)} disabled={!on && selected.length >= max}
                    className="flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left text-sm hover:bg-muted disabled:opacity-50">
                    <span className={cn("flex size-5 items-center justify-center rounded-md border", on ? "border-primary bg-primary text-primary-foreground" : "border-border")}>{on ? <Check className="size-3.5" /> : null}</span>
                    <span className="truncate">{p.name}</span>
                  </button>
                </li>
              );
            })}
            {shown.length === 0 ? <li className="px-2 py-2 text-xs text-muted-foreground">No match.</li> : null}
          </ul>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------- reminders

/** The viewer's own reminders (at most 5). */
export function RemindersEditor({ value, onChange, disabled }: { value: number[]; onChange: (minutes: number[]) => void; disabled?: boolean }) {
  const options = REMINDER_CHOICES.filter((m) => !value.includes(m)).map((m) => ({ value: String(m), label: reminderLabel(m) }));
  return (
    <div className="space-y-2">
      {value.length ? (
        <ul className="flex flex-wrap gap-1.5" aria-label="Reminders">
          {[...value].sort((a, b) => a - b).map((m) => (
            <li key={m} className="inline-flex items-center gap-1.5 rounded-full bg-muted px-3 py-1 text-xs font-medium">
              <Bell className="size-3 text-muted-foreground" aria-hidden />{reminderLabel(m)}
              <button type="button" disabled={disabled} onClick={() => onChange(value.filter((x) => x !== m))} aria-label={`Remove reminder ${reminderLabel(m)}`} className="rounded-full p-0.5 hover:bg-background"><X className="size-3" /></button>
            </li>
          ))}
        </ul>
      ) : <p className="text-xs text-muted-foreground">No reminders.</p>}
      {value.length < LIMITS.reminders && options.length ? (
        <NiceSelect value="" placeholder="Add a reminder" options={options} disabled={disabled} onChange={(v) => onChange([...value, Number(v)])} className="max-w-xs" />
      ) : null}
    </div>
  );
}
