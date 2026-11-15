"use client";

/**
 * Create a meeting (space + optional first occurrence).
 *  - POST /api/v2/meeting-spaces (Idempotency-Key) — `organizationId` only when the selected
 *    workspace grants `meetings.organization.create` (MTG-016 / ORG-010).
 *  - POST /api/v2/meeting-spaces/{id}/conferences (Idempotency-Key) — the first occurrence,
 *    typed in the meeting timezone (MTG-012, MTG-021).
 */
import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCreateSpace } from "@/hooks/v2/useSpaces";
import { useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import { conferencesService } from "@/services/v2/conferences.service";
import { capabilityReasonText } from "@/lib/workspace/workspace-core";
import { occurrenceWindowInZone, validTimeZone, viewerTimeZone } from "@/lib/meeting-time";
import type { AccessPolicy, MeetingSpaceResponse, MeetingType, WaitingRoomPolicy } from "@/types/v2";
import { Button } from "@/components/ui/button";
import { CalendarClock, Check, ChevronDown, ChevronLeft, Globe, GraduationCap, KeyRound, Lock, Megaphone, Presentation, ShieldCheck, Users, Video } from "lucide-react";
import { cn } from "@/lib/utils";
import { TimeZoneField, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";
import { Field, RequestError } from "./ManagementUI";

const meetingTypes: MeetingType[] = ["STANDARD", "WEBINAR", "TOWN_HALL", "TRAINING"];

const ACCESS_OPTIONS: { value: AccessPolicy; label: string; hint: string; icon: typeof Globe; organization?: boolean }[] = [
  { value: "PUBLIC", label: "Anyone with the link", hint: "Easiest for guests", icon: Globe },
  { value: "PUBLIC_PASSCODE", label: "Link + passcode", hint: "Guests also need a code", icon: KeyRound },
  { value: "AUTHENTICATED", label: "Signed-in users", hint: "A CF Meet account is required", icon: Users },
  { value: "INVITE_ONLY", label: "Invite only", hint: "Members and invited people", icon: Lock },
  { value: "ORGANIZATION_ONLY", label: "Organization only", hint: "Members of your organization", icon: ShieldCheck, organization: true },
];

const WAITING_OPTIONS: { value: WaitingRoomPolicy; label: string }[] = [
  { value: "OFF", label: "Off" },
  { value: "GUESTS_ONLY", label: "Guests only" },
  { value: "EVERYONE", label: "Everyone" },
];

const TYPE_LABELS: Record<string, string> = { STANDARD: "Meeting", WEBINAR: "Webinar", TOWN_HALL: "Town hall", TRAINING: "Training" };

const INPUT = "h-11 rounded-xl px-3 py-0 text-sm sm:h-11 sm:px-3 sm:py-0 sm:text-sm";

export default function CreateMeetingSpaceContent() {
  const router = useRouter();
  const create = useCreateSpace();
  const capabilities = useMeetingCapabilities();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [timezone, setTimezone] = useState(() => viewerTimeZone());
  const [type, setType] = useState<MeetingType>("STANDARD");
  const [access, setAccess] = useState<AccessPolicy>("PUBLIC");
  const [waiting, setWaiting] = useState<WaitingRoomPolicy>("OFF");
  const [passcode, setPasscode] = useState("");
  const [owner, setOwner] = useState<"personal" | "organization">("organization");
  const [scheduleNow, setScheduleNow] = useState(true);
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [error, setError] = useState<unknown>();
  const [created, setCreated] = useState<MeetingSpaceResponse | null>(null);
  const [scheduling, setScheduling] = useState(false);
  // One key per user action: a retried submit returns the same space / occurrence (SEC-014).
  const spaceKey = useRef("");
  const conferenceKey = useRef("");

  // Organization meetings only when the selected workspace grants it (default deny).
  const organizationMeeting = capabilities.canCreateOrganization && !!capabilities.organizationId && (owner === "organization" || !capabilities.canCreatePersonal);
  const accessOptions = ACCESS_OPTIONS.filter((option) => !option.organization || organizationMeeting);
  const effectiveAccess = accessOptions.some((option) => option.value === access) ? access : "PUBLIC";
  const zone = validTimeZone(timezone) ? timezone : "UTC";
  const pending = create.isPending || scheduling;

  const scheduleFirst = async (space: MeetingSpaceResponse) => {
    if (!scheduleNow || !start) {
      router.push(`/dashboard/meetings/${space.id}`);
      return;
    }
    setScheduling(true);
    try {
      if (!conferenceKey.current) conferenceKey.current = crypto.randomUUID();
      await conferencesService.create(space.id, occurrenceWindowInZone(start, end || null, space.timezone || zone), conferenceKey.current);
      router.push(`/dashboard/meetings/${space.id}`);
    } catch (cause) {
      setError(cause);
      setCreated(space);
    } finally {
      setScheduling(false);
    }
  };

  if (created) {
    return (
      <div className="mx-auto max-w-xl px-4 py-12 sm:px-6">
        <div className="rounded-3xl border border-border bg-card p-6 sm:p-8">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-amber-500/10 text-amber-600"><CalendarClock className="size-6" /></span>
          <h1 className="mt-5 text-2xl font-bold tracking-tight text-foreground">Almost there</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            &ldquo;{created.title}&rdquo; was created, but its date couldn&apos;t be saved. Try again, or open the meeting and add a date there.
          </p>
          {error !== undefined && <div className="mt-4"><RequestError error={error} /></div>}
          <div className="mt-6 flex flex-wrap gap-2">
            <Button className="h-11 rounded-xl shadow-none" disabled={pending} onClick={() => { setError(undefined); void scheduleFirst(created); }}>{scheduling ? "Saving…" : "Try again"}</Button>
            <Button variant="outline" className="h-11 rounded-xl" asChild><Link href={`/dashboard/meetings/${created.id}`}>Open the meeting</Link></Button>
          </div>
        </div>
      </div>
    );
  }

  const accessChoice = accessOptions.find((option) => option.value === effectiveAccess);
  const whenText = scheduleNow && start
    ? new Date(start).toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })
    : "No date yet";
  const section = "space-y-4 border-t border-border pt-6 first:border-t-0 first:pt-0";
  const sectionTitle = "text-sm font-semibold text-foreground";

  return (
    <div className="mx-auto max-w-6xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <Link href="/dashboard/meetings" className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
        <ChevronLeft className="size-4" /> Meetings
      </Link>
      <h1 className="mt-3 text-xl font-bold tracking-tight text-foreground sm:text-3xl">Schedule a meeting</h1>
      <p className="mt-1 text-sm text-muted-foreground">Set it up once. The meeting keeps its link and settings for next time.</p>

      <div className="mt-6 grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_340px]">
        <form
          className="rounded-3xl border border-border bg-card p-5 sm:p-8"
          onSubmit={async event => {
            event.preventDefault();
            setError(undefined);
            try {
              if (!validTimeZone(timezone)) throw new Error("Choose a valid IANA timezone.");
              if (scheduleNow && start) occurrenceWindowInZone(start, end || null, timezone); // validate before creating anything
              if (!spaceKey.current) spaceKey.current = crypto.randomUUID();
              const space = await create.mutateAsync({
                idempotencyKey: spaceKey.current,
                data: {
                  title: title.trim(),
                  description: description.trim() || undefined,
                  timezone,
                  meetingType: type,
                  accessPolicy: effectiveAccess,
                  waitingRoomPolicy: waiting,
                  ...(organizationMeeting ? { organizationId: capabilities.organizationId as string } : {}),
                  ...(effectiveAccess === "PUBLIC_PASSCODE" ? { passcode: passcode.trim() } : {}),
                },
              });
              await scheduleFirst(space);
            } catch (cause) {
              // A changed form is a new action: an IDEMPOTENCY_CONFLICT must not block it forever.
              if ((cause as { response?: { data?: { error?: { code?: string } } } })?.response?.data?.error?.code === "IDEMPOTENCY_CONFLICT") spaceKey.current = "";
              setError(cause);
            }
          }}
        >
          <fieldset disabled={pending} className="space-y-6">
            {/* What */}
            <div className={section}>
              <p className={sectionTitle}>What</p>
              <Field label="Meeting name" value={title} required maxLength={255} placeholder="e.g. Weekly project sync" onChange={e => setTitle(e.target.value)} className={INPUT} />
              <label className="grid gap-2 text-sm font-medium text-foreground">
                Description <span className="-mt-1 text-xs font-normal text-muted-foreground">Optional</span>
                <textarea
                  value={description}
                  rows={3}
                  placeholder="Agenda, links or notes for attendees"
                  onChange={e => setDescription(e.target.value)}
                  className="min-h-[88px] rounded-xl border border-transparent bg-muted/70 focus:bg-background px-3 py-2.5 text-sm text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                />
              </label>
            </div>

            {/* When */}
            <div className={section}>
              <div className="flex items-center justify-between gap-3">
                <p className={sectionTitle}>When</p>
                <label className="flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
                  Set a date
                  <button
                    type="button"
                    role="switch"
                    aria-checked={scheduleNow}
                    onClick={() => setScheduleNow(v => !v)}
                    className={cn("relative h-6 w-11 rounded-full transition-colors", scheduleNow ? "bg-primary" : "bg-muted-foreground/30")}
                  >
                    <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", scheduleNow ? "left-[22px]" : "left-0.5")} />
                  </button>
                </label>
              </div>
              {scheduleNow ? (
                <div className="grid gap-4 md:grid-cols-2">
                  <ZonedDateTimeField id="create-start" label="Starts" value={start} onChange={setStart} timeZone={zone} />
                  <ZonedDateTimeField id="create-end" label="Ends" value={end} min={start || undefined} disabled={!start} onChange={setEnd} timeZone={zone} />
                </div>
              ) : (
                <p className="rounded-xl bg-muted/60 px-4 py-3 text-sm text-muted-foreground">No date for now. You can add dates or a repeating series later from the meeting.</p>
              )}
              <TimeZoneField id="create-timezone" label="Time zone" value={timezone} onChange={setTimezone} />
            </div>

            {/* Who */}
            <div className={section}>
              <p className={sectionTitle}>Who can join</p>
              {capabilities.organizationId ? (
                <div className="flex flex-wrap gap-2">
                  {capabilities.canCreateOrganization ? (
                    <button type="button" onClick={() => setOwner("organization")} className={cn("h-10 rounded-xl border px-4 text-sm font-medium transition-colors", organizationMeeting ? "border-primary bg-primary/10 text-primary" : "border-border text-foreground hover:bg-muted")}>
                      {capabilities.organizationName ?? "This organization"}
                    </button>
                  ) : null}
                  {capabilities.canCreatePersonal ? (
                    <button type="button" onClick={() => setOwner("personal")} className={cn("h-10 rounded-xl border px-4 text-sm font-medium transition-colors", !organizationMeeting ? "border-primary bg-primary/10 text-primary" : "border-border text-foreground hover:bg-muted")}>
                      Just me
                    </button>
                  ) : null}
                  {!capabilities.canCreateOrganization ? <p className="w-full text-xs text-muted-foreground">Organization meetings: {capabilityReasonText(capabilities.organizationCreateDecision.reason)}</p> : null}
                </div>
              ) : null}
              <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label="Who can join">
                {accessOptions.map(({ value, label, hint, icon: Icon }) => {
                  const selected = effectiveAccess === value;
                  return (
                    <button
                      key={value}
                      type="button"
                      role="radio"
                      aria-checked={selected}
                      onClick={() => setAccess(value)}
                      className={cn(
                        "flex items-start gap-3 rounded-xl border p-3.5 text-left transition-colors",
                        selected ? "border-primary bg-primary/5 ring-1 ring-primary" : "border-border hover:bg-muted/60",
                      )}
                    >
                      <Icon className={cn("mt-0.5 size-4 shrink-0", selected ? "text-primary" : "text-muted-foreground")} aria-hidden />
                      <span>
                        <span className="block text-sm font-medium text-foreground">{label}</span>
                        <span className="block text-xs text-muted-foreground">{hint}</span>
                      </span>
                    </button>
                  );
                })}
              </div>
              {effectiveAccess === "PUBLIC_PASSCODE" && (
                <Field label="Passcode" type="password" minLength={4} maxLength={64} required value={passcode} placeholder="At least 4 characters" onChange={e => setPasscode(e.target.value)} className={INPUT} />
              )}
            </div>

            {/* Options */}
            <div className={section}>
              <p className={sectionTitle}>Options</p>
              <div className="grid gap-4 md:grid-cols-2">
                <div className="grid gap-2">
                  <span className="text-sm font-medium text-foreground">Waiting room</span>
                  <div className="grid grid-cols-3 rounded-xl bg-muted p-1" role="radiogroup" aria-label="Waiting room">
                    {WAITING_OPTIONS.map(o => (
                      <button
                        key={o.value}
                        type="button"
                        role="radio"
                        aria-checked={waiting === o.value}
                        onClick={() => setWaiting(o.value)}
                        className={cn("h-9 rounded-lg text-sm font-medium transition-colors", waiting === o.value ? "bg-card text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}
                      >
                        {o.label}
                      </button>
                    ))}
                  </div>
                </div>
                <TypeSelect value={type} onChange={setType} />
              </div>
            </div>

            {error !== undefined && <RequestError error={error} />}

            <div className="flex flex-col-reverse gap-2 border-t border-border pt-6 sm:flex-row sm:justify-end">
              <Button type="button" variant="outline" className="h-11 rounded-xl" asChild>
                <Link href="/dashboard/meetings">Cancel</Link>
              </Button>
              <Button disabled={pending || !title.trim() || (!capabilities.canCreatePersonal && !organizationMeeting)} type="submit" className="h-11 rounded-xl px-6 shadow-none hover:brightness-100">
                {create.isPending ? "Saving…" : scheduling ? "Scheduling…" : scheduleNow && start ? "Schedule meeting" : "Create meeting"}
              </Button>
            </div>
          </fieldset>
        </form>

        {/* Live summary */}
        <aside className="rounded-3xl border border-border bg-card p-6 lg:sticky lg:top-6">
          <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Preview</p>
          <div className="mt-4 flex items-start gap-3">
            <span className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><Video className="size-5" /></span>
            <div className="min-w-0">
              <p className={cn("break-words font-semibold", title.trim() ? "text-foreground" : "text-muted-foreground")}>{title.trim() || "Untitled meeting"}</p>
              <p className="text-sm text-muted-foreground">{TYPE_LABELS[type] ?? type} · {organizationMeeting ? capabilities.organizationName ?? "Organization" : "Personal"}</p>
            </div>
          </div>
          <dl className="mt-5 space-y-3 border-t border-border pt-5 text-sm">
            <div className="flex items-start gap-3">
              <dt className="sr-only">When</dt>
              <CalendarClock className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
              <dd className="text-foreground">{whenText}<span className="block text-xs text-muted-foreground">{zone.replaceAll("_", " ")}</span></dd>
            </div>
            <div className="flex items-start gap-3">
              <dt className="sr-only">Who can join</dt>
              {accessChoice ? <accessChoice.icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden /> : null}
              <dd className="text-foreground">{accessChoice?.label}</dd>
            </div>
            <div className="flex items-start gap-3">
              <dt className="sr-only">Waiting room</dt>
              <ShieldCheck className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
              <dd className="text-foreground">Waiting room: {WAITING_OPTIONS.find(o => o.value === waiting)?.label.toLowerCase()}</dd>
            </div>
          </dl>
          <p className="mt-5 rounded-xl bg-muted/60 p-3 text-xs leading-relaxed text-muted-foreground">
            You&apos;ll get a link to share right after saving. Invites, repeating dates and members are set up on the meeting page.
          </p>
        </aside>
      </div>
    </div>
  );
}

const TYPE_DETAILS: Record<string, { label: string; hint: string; icon: typeof Video }> = {
  STANDARD: { label: "Meeting", hint: "Everyone can talk and share", icon: Video },
  WEBINAR: { label: "Webinar", hint: "Presenters speak, the audience watches", icon: Presentation },
  TOWN_HALL: { label: "Town hall", hint: "Large all-hands with Q&A", icon: Megaphone },
  TRAINING: { label: "Training", hint: "Teach a group with polls and breakouts", icon: GraduationCap },
};

/** Meeting type as a designed dropdown: icon, name and one-line meaning for each option. */
function TypeSelect({ value, onChange }: { value: MeetingType; onChange: (v: MeetingType) => void }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const current = TYPE_DETAILS[value] ?? TYPE_DETAILS.STANDARD;
  const CurrentIcon = current.icon;

  useEffect(() => {
    if (!open) return;
    setActive(Math.max(0, meetingTypes.indexOf(value)));
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open, value]);

  const pick = (v: MeetingType | undefined) => {
    if (!v) return;
    onChange(v);
    setOpen(false);
  };

  return (
    <div className="grid gap-2" ref={ref}>
      <span id="type-label" className="text-sm font-medium text-foreground">Meeting type</span>
      <div className="relative">
        <button
          type="button"
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-labelledby="type-label"
          onClick={() => setOpen((o) => !o)}
          onKeyDown={(e) => {
            if (!open && (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ")) { e.preventDefault(); setOpen(true); return; }
            if (!open) return;
            if (e.key === "Escape") setOpen(false);
            else if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => Math.min(a + 1, meetingTypes.length - 1)); }
            else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
            else if (e.key === "Enter") { e.preventDefault(); pick(meetingTypes[active]); }
          }}
          className="flex h-11 w-full items-center gap-2.5 rounded-xl border border-transparent bg-muted/70 focus:bg-background px-3 text-left text-sm transition-colors hover:bg-muted/50 focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
        >
          <CurrentIcon className="size-4 shrink-0 text-primary" aria-hidden />
          <span className="min-w-0 flex-1 truncate font-medium text-foreground">{current.label}</span>
          <ChevronDown className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} aria-hidden />
        </button>
        {open && (
          <ul role="listbox" aria-labelledby="type-label" className="absolute left-0 right-0 top-full z-40 mt-2 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
            {meetingTypes.map((t, i) => {
              const d = TYPE_DETAILS[t] ?? { label: t, hint: "", icon: Video };
              const Icon = d.icon;
              const selected = t === value;
              return (
                <li key={t} role="option" aria-selected={selected}>
                  <button
                    type="button"
                    onMouseEnter={() => setActive(i)}
                    onClick={() => pick(t)}
                    className={cn("flex w-full items-center gap-3 rounded-lg px-2.5 py-2.5 text-left", i === active && "bg-muted")}
                  >
                    <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-lg", selected ? "bg-primary text-primary-foreground" : "bg-primary/10 text-primary")}>
                      <Icon className="size-4" aria-hidden />
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-medium text-foreground">{d.label}</span>
                      <span className="block truncate text-xs text-muted-foreground">{d.hint}</span>
                    </span>
                    {selected && <Check className="size-4 shrink-0 text-primary" aria-hidden />}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
