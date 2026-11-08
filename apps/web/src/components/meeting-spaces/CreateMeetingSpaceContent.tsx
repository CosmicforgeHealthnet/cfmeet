"use client";

/**
 * Create a meeting (space + optional first occurrence).
 *  - POST /api/v2/meeting-spaces (Idempotency-Key) — `organizationId` only when the selected
 *    workspace grants `meetings.organization.create` (MTG-016 / ORG-010).
 *  - POST /api/v2/meeting-spaces/{id}/conferences (Idempotency-Key) — the first occurrence,
 *    typed in the meeting timezone (MTG-012, MTG-021).
 */
import { useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCreateSpace } from "@/hooks/v2/useSpaces";
import { useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import { conferencesService } from "@/services/v2/conferences.service";
import { capabilityReasonText } from "@/lib/workspace/workspace-core";
import { occurrenceWindowInZone, validTimeZone, viewerTimeZone } from "@/lib/meeting-time";
import type { AccessPolicy, MeetingSpaceResponse, MeetingType, WaitingRoomPolicy } from "@/types/v2";
import { Button } from "@/components/ui/button";
import { TimeZoneField, ZonedDateTimeField } from "@/components/meetings/ScheduleFields";
import { Field, RequestError } from "./ManagementUI";

const meetingTypes: MeetingType[] = ["STANDARD", "WEBINAR", "TOWN_HALL", "TRAINING"];

const ACCESS_OPTIONS: { value: AccessPolicy; label: string; organization?: boolean }[] = [
  { value: "PUBLIC", label: "Public" },
  { value: "PUBLIC_PASSCODE", label: "Passcode protected" },
  { value: "AUTHENTICATED", label: "Signed-in users" },
  { value: "INVITE_ONLY", label: "Members and invitations" },
  { value: "ORGANIZATION_ONLY", label: "Organization members only", organization: true },
];

const SELECT = "h-10 rounded-md border border-input bg-background px-3 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40";

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
    return <div className="mx-auto max-w-xl space-y-4 px-4 py-10">
      <h1 className="text-2xl font-bold">Meeting space created</h1>
      <p className="text-sm text-muted-foreground">“{created.title}” was created, but its first occurrence could not be scheduled.</p>
      {error !== undefined && <RequestError error={error} />}
      <div className="flex flex-wrap gap-2">
        <Button disabled={pending} onClick={() => { setError(undefined); void scheduleFirst(created); }}>{scheduling ? "Scheduling…" : "Retry scheduling"}</Button>
        <Button variant="outline" asChild><Link href={`/dashboard/meetings/${created.id}`}>Open the meeting space</Link></Button>
      </div>
    </div>;
  }

  return (
    <div className="mx-auto max-w-6xl px-4 py-6 sm:px-6 sm:py-8 md:px-10 md:py-10">
      <div className="mb-6 flex items-center justify-between gap-3">
        <Button variant="outline" asChild>
          <Link href="/dashboard/meetings">Back to meetings</Link>
        </Button>
      </div>

      <div className="grid gap-6 lg:grid-cols-[1.25fr_0.75fr]">
        <section className="rounded-2xl border border-border bg-card p-5 shadow-sm sm:p-6">
          <header className="mb-6">
            <p className="mb-2 text-xs font-medium uppercase tracking-[0.18em] text-primary">Setup</p>
            <h1 className="text-2xl font-bold tracking-tight text-foreground sm:text-3xl">Schedule a meeting</h1>
            <p className="mt-2 max-w-xl text-sm text-muted-foreground">
              A meeting space keeps the settings and the link. Pick its first date and time now, or schedule occurrences and a recurring series later from the space.
            </p>
          </header>

          <form
            className="space-y-5"
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
            <fieldset disabled={pending} className="space-y-5">
              {capabilities.organizationId ? (
                <div className="grid gap-2 text-sm">
                  <span className="font-medium">Meeting for</span>
                  <div className="flex flex-wrap gap-4">
                    {capabilities.canCreateOrganization ? <label className="flex items-center gap-2"><input type="radio" name="owner" checked={organizationMeeting} onChange={() => setOwner("organization")} /> {capabilities.organizationName ?? "This organization"}</label> : null}
                    {capabilities.canCreatePersonal ? <label className="flex items-center gap-2"><input type="radio" name="owner" checked={!organizationMeeting} onChange={() => setOwner("personal")} /> Me (personal)</label> : null}
                  </div>
                  {!capabilities.canCreateOrganization ? <p className="text-xs text-muted-foreground">Organization meetings: {capabilityReasonText(capabilities.organizationCreateDecision.reason)}</p> : null}
                </div>
              ) : null}

              <div className="grid gap-5 md:grid-cols-2">
                <Field label="Meeting name" value={title} required maxLength={255} placeholder="Weekly project sync" onChange={e => setTitle(e.target.value)} />
                <TimeZoneField id="create-timezone" value={timezone} onChange={setTimezone} />
              </div>

              <label className="grid gap-2 text-sm font-medium text-foreground">
                Description
                <textarea
                  value={description}
                  rows={3}
                  placeholder="Agenda, links or notes for attendees."
                  onChange={e => setDescription(e.target.value)}
                  className="min-h-[80px] rounded-md border border-input bg-background px-3 py-2 text-sm text-foreground shadow-sm transition-colors placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
                />
              </label>

              <div className="space-y-3 rounded-xl border border-border p-4">
                <label className="flex items-center gap-2 text-sm font-medium"><input type="checkbox" checked={scheduleNow} onChange={e => setScheduleNow(e.target.checked)} /> Schedule the first occurrence now</label>
                {scheduleNow ? <div className="grid gap-4 md:grid-cols-2">
                  <ZonedDateTimeField id="create-start" label="Starts" value={start} onChange={setStart} timeZone={zone} />
                  <ZonedDateTimeField id="create-end" label="Ends (optional)" value={end} min={start || undefined} disabled={!start} onChange={setEnd} timeZone={zone} />
                  <p className="text-xs text-muted-foreground md:col-span-2">Leave the start empty to create the space without a date. Recurring series are set up on the space page.</p>
                </div> : null}
              </div>

              <div className="grid gap-5 md:grid-cols-2">
                <label className="grid gap-2 text-sm font-medium text-foreground">
                  Meeting type
                  <select className={SELECT} value={type} onChange={e => setType(e.target.value as MeetingType)}>
                    {meetingTypes.map(value => <option key={value} value={value}>{value.replaceAll("_", " ")}</option>)}
                  </select>
                </label>
                <label className="grid gap-2 text-sm font-medium text-foreground">
                  Waiting room
                  <select className={SELECT} value={waiting} onChange={e => setWaiting(e.target.value as WaitingRoomPolicy)}>
                    <option value="OFF">Off</option>
                    <option value="GUESTS_ONLY">Guests only</option>
                    <option value="EVERYONE">Everyone</option>
                  </select>
                </label>
              </div>

              <label className="grid gap-2 text-sm font-medium text-foreground">
                Who can join
                <select className={SELECT} value={effectiveAccess} onChange={e => setAccess(e.target.value as AccessPolicy)}>
                  {accessOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                </select>
              </label>

              {effectiveAccess === "PUBLIC_PASSCODE" && (
                <Field label="Passcode" type="password" minLength={4} maxLength={64} required value={passcode} placeholder="Enter a secure passcode" onChange={e => setPasscode(e.target.value)} />
              )}

              <div className="flex flex-col gap-3 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between">
                <p className="text-sm text-muted-foreground">
                  {title.trim() ? `Ready to create “${title.trim()}”` : "Add a meeting name to continue"}
                </p>
                <Button disabled={pending || !title.trim() || (!capabilities.canCreatePersonal && !organizationMeeting)} type="submit" className="min-w-[180px]">
                  {create.isPending ? "Creating…" : scheduling ? "Scheduling…" : scheduleNow && start ? "Create and schedule" : "Create meeting space"}
                </Button>
              </div>
            </fieldset>

            {error !== undefined && <RequestError error={error} />}
          </form>
        </section>

        <aside className="rounded-2xl border border-border bg-gradient-to-b from-primary/5 via-background to-background p-5 shadow-sm sm:p-6">
          <p className="text-xs font-medium uppercase tracking-[0.18em] text-primary">Summary</p>
          <div className="mt-5 space-y-4">
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Owner</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{organizationMeeting ? capabilities.organizationName ?? "Organization" : "Personal"}</p>
            </div>
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Meeting type</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{type.replaceAll("_", " ")}</p>
            </div>
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Who can join</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{accessOptions.find((option) => option.value === effectiveAccess)?.label}</p>
            </div>
          </div>

          <div className="mt-6 rounded-xl border border-dashed border-primary/30 bg-primary/5 p-4 text-sm text-muted-foreground">
            Times are entered in the meeting timezone ({zone}) and shown to attendees in that zone and their own. Members, invitations, recurrence and live controls are managed from the space page.
          </div>
        </aside>
      </div>
    </div>
  );
}
