"use client";

import { useState } from "react";
import Link from "next/link";
import { CalendarClock, CalendarPlus, Check, ChevronLeft, Copy, Globe, KeyRound, Lock, ShieldCheck, UserPlus, Users, Video } from "lucide-react";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useCalendarDownload } from "@/hooks/v2/useCalendar";
import MeetingSpaceActions from "@/components/dashboard/MeetingSpaceActions";
import ConferencesPanel from "@/components/conferences/ConferencesPanel";
import SpaceSchedule from "@/components/meetings/SpaceSchedule";
import { cn } from "@/lib/utils";
import { meetingAllows } from "@/lib/meeting-authority";
import { conferencePermission } from "@/lib/meeting-scheduling";
import RecurrencePanel from "./RecurrencePanel";
import MembersPanel from "./MembersPanel";
import InvitationsPanel from "./InvitationsPanel";
import RegistrationPanel from "./RegistrationPanel";
import { RequestError } from "./ManagementUI";
import SpaceMeetingRules from "./SpaceMeetingRules";

const TYPE_LABELS: Record<string, string> = { STANDARD: "Meeting", WEBINAR: "Webinar", TOWN_HALL: "Town hall", HEALTHCARE: "Healthcare", TRAINING: "Training" };
const ACCESS: Record<string, { label: string; icon: typeof Globe }> = {
  PUBLIC: { label: "Anyone with the link", icon: Globe },
  PUBLIC_PASSCODE: { label: "Link + passcode", icon: KeyRound },
  AUTHENTICATED: { label: "Signed-in users", icon: Users },
  ORGANIZATION_ONLY: { label: "Organization only", icon: ShieldCheck },
  DOMAIN_RESTRICTED: { label: "Domain only", icon: ShieldCheck },
  INVITE_ONLY: { label: "Invite only", icon: Lock },
  CUSTOM: { label: "Custom access", icon: Users },
  HEALTHCARE_APPOINTMENT: { label: "Appointment", icon: ShieldCheck },
};
const WAITING: Record<string, string> = { OFF: "Off", GUESTS_ONLY: "Guests only", EVERYONE: "Everyone" };

type Tab = "overview" | "sessions" | "schedule" | "people";

export default function MeetingSpaceWorkspace({ id }: { id: string }) {
  const query = useSpace(id);
  const space = query.data;
  const download = useCalendarDownload();
  // "Edit series" links (…#recurrence) open the Schedule tab directly.
  const [tab, setTab] = useState<Tab>(() => (typeof window !== "undefined" && window.location.hash === "#recurrence" ? "schedule" : "overview"));
  const [copied, setCopied] = useState(false);

  const link = typeof window !== "undefined" && space ? `${window.location.origin}/${space.meetingCode}` : "";
  const copy = () => {
    void navigator.clipboard.writeText(link);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };
  const canManagePeople = !!space?.viewerRole && ["OWNER", "HOST", "CO_HOST"].includes(space.viewerRole);
  // People has content only for those who can invite/admit or manage members; hide it otherwise (no empty tab).
  const showPeople = !!space && (canManagePeople || meetingAllows(space.viewerRole, "participant.admit", space.rolePolicy));
  const archived = space?.status === "ARCHIVED";
  // The main action follows the real state of the latest session.
  const latestStatus = space?.latestConference?.status;
  const isLive = latestStatus === "LIVE";
  const isUpcoming = !!latestStatus && ["CREATED", "PREPARING", "WAITING"].includes(latestStatus);
  const canStart = !!space && conferencePermission(space, "start");
  const access = space ? ACCESS[space.accessPolicy] ?? { label: space.accessPolicy, icon: Users } : null;

  const tabs: { id: Tab; label: string }[] = [
    { id: "overview", label: "Overview" },
    { id: "sessions", label: "Sessions" },
    { id: "schedule", label: "Schedule" },
    ...(showPeople ? [{ id: "people" as Tab, label: "People" }] : []),
  ];

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <Link href="/dashboard/meetings" className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
        <ChevronLeft className="size-4" /> Meetings
      </Link>

      {query.isLoading && (
        <div className="mt-4 space-y-3" aria-busy="true" aria-label="Loading meeting">
          <span className="block h-8 w-1/3 animate-pulse rounded bg-muted" />
          <span className="block h-4 w-1/4 animate-pulse rounded bg-muted" />
          <span className="mt-6 block h-48 animate-pulse rounded-2xl bg-muted" />
        </div>
      )}
      {query.isError && <div className="mt-4"><RequestError error={query.error} retry={() => void query.refetch()} /></div>}

      {space && (
        <>
          {/* Header */}
          <header className="mt-3 flex flex-col gap-5 lg:flex-row lg:items-start lg:justify-between">
            <div className="min-w-0">
              <h1 className="break-words text-xl font-bold tracking-tight text-foreground sm:text-3xl">{space.title || "Untitled meeting"}</h1>
              <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
                <span className="rounded-md bg-primary/10 px-2 py-0.5 font-medium text-primary">{TYPE_LABELS[space.meetingType] ?? space.meetingType}</span>
                {access && (
                  <span className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 font-medium text-muted-foreground">
                    <access.icon className="size-3" aria-hidden /> {access.label}
                  </span>
                )}
                {space.organizationId && <span className="rounded-md bg-violet-500/10 px-2 py-0.5 font-medium text-violet-600 dark:text-violet-300">Organization</span>}
                {archived && <span className="rounded-md bg-muted px-2 py-0.5 font-medium text-muted-foreground">Archived</span>}
              </div>
              <div className="mt-2 text-sm text-muted-foreground"><SpaceSchedule space={space} /></div>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              {!archived && (isLive || isUpcoming) && (
                <Link
                  href={`/conferences/${encodeURIComponent(space.latestConference!.id)}`}
                  className={cn(
                    "inline-flex h-11 items-center gap-2 rounded-xl px-5 text-sm font-semibold text-white transition-colors",
                    isLive ? "bg-[var(--success)] hover:bg-[var(--success)]/90" : "bg-primary hover:bg-primary/90",
                  )}
                >
                  {isLive ? <span className="size-2 animate-pulse rounded-full bg-white" /> : <Video className="size-4" />}
                  {isLive ? "Join now" : canStart ? "Start meeting" : "Open"}
                </Link>
              )}
              {!archived && !isLive && !isUpcoming && canStart && (
                <button
                  type="button"
                  onClick={() => setTab("sessions")}
                  className="inline-flex h-11 items-center gap-2 rounded-xl bg-primary px-5 text-sm font-semibold text-primary-foreground transition-colors hover:bg-primary/90"
                >
                  <CalendarPlus className="size-4" /> Add a session
                </button>
              )}
{!archived && (
              <button
                type="button"
                onClick={copy}
                className="inline-flex h-11 items-center gap-2 rounded-xl border border-border bg-card px-4 text-sm font-semibold text-foreground transition-colors hover:bg-muted"
              >
                {copied ? <Check className="size-4 text-[var(--success)]" /> : <Copy className="size-4" />}
                {copied ? "Copied" : "Copy link"}
              </button>
              )}
              <MeetingSpaceActions
                space={space}
                extraItems={[
                  ...(showPeople && !archived ? [{ label: "Invite people", icon: UserPlus, run: () => setTab("people") }] : []),
                  ...(space.viewerRole ? [{ label: "Add to calendar", icon: CalendarPlus, run: () => download.mutate({ kind: "space", id: space.id, name: space.title || space.meetingCode }) }] : []),
                ]}
              />
            </div>
          </header>
          {download.isError ? <div className="mt-3"><RequestError error={download.error} /></div> : null}

          {!space.viewerRole ? (
            <p className="mt-6 rounded-2xl border border-border bg-card p-6 text-sm text-muted-foreground">
              You can see this meeting&apos;s summary only. A link or code doesn&apos;t give access to its sessions, schedule or people.
            </p>
          ) : (
            <>
              {/* Tabs */}
              <div className="mt-6 flex gap-6 overflow-x-auto border-b border-border" role="tablist" aria-label="Meeting sections">
                {tabs.map((t) => (
                  <button
                    key={t.id}
                    type="button"
                    role="tab"
                    aria-selected={tab === t.id}
                    onClick={() => setTab(t.id)}
                    className={cn(
                      "-mb-px h-11 shrink-0 border-b-2 text-sm font-medium transition-colors",
                      tab === t.id ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {t.label}
                  </button>
                ))}
              </div>

              <div className="mt-6" role="tabpanel">
                {tab === "overview" && (
                  <div>
                    <section className="rounded-2xl border border-border bg-card p-6">
                      <h2 className="text-base font-semibold text-foreground">Details</h2>
                      <dl className="mt-4 grid gap-x-6 gap-y-5 sm:grid-cols-2">
                        <Detail icon={CalendarClock} label="When"><SpaceSchedule space={space} /></Detail>
                        <Detail icon={Globe} label="Time zone">{(space.timezone || "UTC").replaceAll("_", " ")}</Detail>
                        <Detail icon={access?.icon ?? Users} label="Who can join">{access?.label}</Detail>
                        <Detail icon={ShieldCheck} label="Waiting room">{WAITING[space.waitingRoomPolicy ?? "OFF"] ?? space.waitingRoomPolicy}</Detail>
                      </dl>
                      {space.description ? (
                        <div className="mt-6 border-t border-border pt-5">
                          <p className="text-xs font-medium text-muted-foreground">Description</p>
                          <p className="mt-1.5 whitespace-pre-wrap text-sm text-foreground">{space.description}</p>
                        </div>
                      ) : null}
                    </section>
                    {space.viewerRole && ["OWNER", "HOST"].includes(space.viewerRole) ? <SpaceMeetingRules space={space} /> : null}
                  </div>
                )}

                {tab === "sessions" && <ConferencesPanel space={space} />}
                {tab === "schedule" && <div id="recurrence" className="scroll-mt-20"><RecurrencePanel space={space} /></div>}
                {tab === "people" && showPeople && (
                  <div className="space-y-8">
                    <InvitationsPanel space={space} />
                    <RegistrationPanel space={space} />
                    {canManagePeople && <MembersPanel space={space} />}
                  </div>
                )}
              </div>
            </>
          )}
        </>
      )}
    </div>
  );
}

function Detail({ icon: Icon, label, children }: { icon: typeof Globe; label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
        <Icon className="size-4" aria-hidden />
      </span>
      <div className="min-w-0">
        <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
        <dd className="mt-0.5 text-sm text-foreground">{children}</dd>
      </div>
    </div>
  );
}
