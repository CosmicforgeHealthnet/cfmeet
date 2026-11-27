"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import type { LucideIcon } from "lucide-react";
import {
  Archive,
  ArrowRight,
  CalendarClock,
  CalendarPlus,
  Check,
  Copy,
  Link2,
  Loader2,
  MessageSquarePlus,
  Phone,
  QrCode,
  Video,
} from "lucide-react";
import { useArchiveSpace, useCreateInstantMeeting, useSpaces } from "@/hooks/v2/useSpaces";
import { spacePermissions } from "@/lib/meeting-spaces";
import { useMeetingCapabilities, useUpcomingMeetings } from "@/hooks/v2/useCalendar";
import { useProfile } from "@/hooks";
import type { MeetingSpaceResponse } from "@/types/v2";
import type { AgendaItem } from "@/types/generated/meeting-service";
import { agendaItemKey } from "@/lib/meeting-calendar";
import { viewerTimeZone } from "@/lib/meeting-time";
import { getErrorMessage } from "@/utils/error";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";
import OccurrenceDialog from "@/components/meetings/OccurrenceDialog";
import { cn } from "@/lib/utils";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { CalendarArt, RoomArt } from "./EmptyArt";

function useUpcomingAgenda() {
  const { organizationId } = useMeetingCapabilities();
  return useUpcomingMeetings(6, organizationId);
}

function greeting(now: Date) {
  const h = now.getHours();
  return h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
}

/** Card shell shared by every Home widget: each one loads and fails on its own. */
function Panel({ title, action, children, className }: { title: string; action?: React.ReactNode; children: React.ReactNode; className?: string }) {
  return (
    <section className={cn("rounded-2xl border border-border bg-card", className)}>
      <div className="flex items-center justify-between gap-3 px-5 pb-1 pt-4">
        <h2 className="text-base font-semibold text-foreground">{title}</h2>
        {action}
      </div>
      <div className="p-5 pt-3">{children}</div>
    </section>
  );
}

function EmptyState({ icon: Icon, art, title, body, children }: { icon: LucideIcon; art?: React.ReactNode; title: string; body: string; children?: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center px-4 py-8 text-center">
      {art ?? (
        <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">
          <Icon className="size-6" aria-hidden />
        </span>
      )}
      <p className="mt-4 font-semibold text-foreground">{title}</p>
      <p className="mt-1 max-w-xs text-sm text-muted-foreground">{body}</p>
      {children && <div className="mt-5 flex flex-wrap justify-center gap-2">{children}</div>}
    </div>
  );
}

function SkeletonRows({ rows = 3 }: { rows?: number }) {
  return (
    <ul className="space-y-3" aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }, (_, i) => (
        <li key={i} className="flex items-center gap-3">
          <span className="h-12 w-14 animate-pulse rounded-xl bg-muted" />
          <span className="flex-1 space-y-2">
            <span className="block h-3.5 w-2/3 animate-pulse rounded bg-muted" />
            <span className="block h-3 w-1/3 animate-pulse rounded bg-muted" />
          </span>
        </li>
      ))}
    </ul>
  );
}

const smallButton =
  "inline-flex h-10 items-center gap-1.5 rounded-xl px-4 text-sm font-semibold transition-colors";

export default function DashboardContent() {
  const router = useRouter();
  const { data: user } = useProfile();
  const createInstantMeeting = useCreateInstantMeeting();
  const [instantError, setInstantError] = useState("");
  const capabilities = useMeetingCapabilities();
  const canSchedule = capabilities.canCreatePersonal || capabilities.canCreateOrganization;
  const [now] = useState(() => new Date());

  const startInstant = async () => {
    if (createInstantMeeting.isPending) return;
    setInstantError("");
    try {
      // The live room is keyed by the conference id (FE-017); the code stays the share link.
      const { conference } = await createInstantMeeting.mutateAsync({ timezone: viewerTimeZone() });
      router.push(`/conferences/${encodeURIComponent(conference.id)}`);
    } catch (error) {
      setInstantError(getErrorMessage(error, "Could not start an instant meeting."));
    }
  };

  const shortcuts: { label: string; icon: LucideIcon; tone: string; href: string; show: boolean }[] = [
    { label: "Schedule", icon: CalendarPlus, tone: "bg-violet-500/10 text-violet-600 dark:text-violet-300", href: "/dashboard/meetings/new", show: canSchedule },
    { label: "Message", icon: MessageSquarePlus, tone: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-300", href: "/dashboard/messages", show: true },
    { label: "Call", icon: Phone, tone: "bg-amber-500/10 text-amber-600 dark:text-amber-300", href: "/calling", show: true },
  ];

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Hero: primary actions + next meeting */}
      <section className="grid gap-4 lg:grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)]">
        <div className="rounded-3xl border border-border bg-card p-5 sm:p-8">
          <p className="text-sm text-muted-foreground">
            {now.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" })}
          </p>
          <h1 className="mt-1 text-2xl font-bold tracking-tight text-foreground sm:text-3xl">
            {greeting(now)}{user?.username ? `, ${user.username}` : ""}
          </h1>

          <div className="mt-6 flex flex-col gap-3 sm:flex-row">
            {capabilities.canCreatePersonal && (
              <button
                type="button"
                onClick={() => void startInstant()}
                disabled={createInstantMeeting.isPending}
                aria-busy={createInstantMeeting.isPending}
                className="inline-flex h-12 shrink-0 items-center justify-center gap-2 rounded-xl bg-primary px-5 font-semibold text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-wait disabled:opacity-80"
              >
                {createInstantMeeting.isPending ? <Loader2 className="size-5 animate-spin" /> : <Video className="size-5" />}
                {createInstantMeeting.isPending ? "Starting…" : "New meeting"}
              </button>
            )}
            <div className="min-w-0 flex-1">
              <label htmlFor="home-join" className="sr-only">Meeting code or link</label>
              <JoinMeetingInput
                inputId="home-join"
                placeholder="Enter a code or link"
                inputClassName="h-12 rounded-xl px-4 py-0 text-base sm:h-12 sm:px-4 sm:py-0"
                buttonClassName="h-12 rounded-xl px-5 shadow-none disabled:bg-muted disabled:text-muted-foreground disabled:opacity-100"
              />
            </div>
          </div>
          {instantError && <p role="alert" className="mt-3 text-sm text-destructive">{instantError}</p>}

          <div className="mt-6 flex flex-wrap items-end gap-x-6 gap-y-4 border-t border-border pt-6">
            {shortcuts.filter((a) => a.show).map((a) => {
              const Icon = a.icon;
              return (
                <Link key={a.label} href={a.href} className="group flex flex-col items-center gap-2 text-center">
                  <span className={cn("flex size-12 items-center justify-center rounded-2xl transition-transform group-hover:-translate-y-0.5", a.tone)}>
                    <Icon className="size-5" aria-hidden />
                  </span>
                  <span className="text-xs font-medium text-foreground">{a.label}</span>
                </Link>
              );
            })}
            <Link
              href="/dashboard/contacts"
              className="group flex w-full items-center justify-center gap-2 rounded-xl border border-border px-3 py-2.5 text-sm font-medium text-foreground transition-colors hover:bg-muted sm:ml-auto sm:w-auto sm:py-2"
            >
              <QrCode className="size-4 text-primary" aria-hidden />
              Your CF Meet ID
              <ArrowRight className="size-3.5 text-muted-foreground transition-transform group-hover:translate-x-0.5" aria-hidden />
            </Link>
          </div>
        </div>

        <NextMeeting />
      </section>

      {/* Day + spaces */}
      <div className="mt-6 space-y-6">
        <UpNext canSchedule={canSchedule} onStart={() => void startInstant()} canStart={capabilities.canCreatePersonal} />
        <Spaces canSchedule={canSchedule} />
      </div>
    </div>
  );
}

function UpNext({ canSchedule, canStart, onStart }: { canSchedule: boolean; canStart: boolean; onStart: () => void }) {
  const router = useRouter();
  const agenda = useUpcomingAgenda();
  const [selected, setSelected] = useState<AgendaItem | null>(null);
  const forbidden = (agenda.error as { response?: { status?: number } } | null)?.response?.status === 403;
  // The first item is featured in the Next meeting card; list the rest.
  const items = (agenda.data?.items ?? []).slice(1);

  return (
    <Panel
      title="Later this week"
      action={
        <Link href="/dashboard/calendar" className="flex items-center gap-1 text-sm font-semibold text-primary hover:underline">
          Calendar <ArrowRight className="size-3.5" />
        </Link>
      }
    >
      {agenda.isLoading ? (
        <SkeletonRows />
      ) : forbidden ? (
        <EmptyState icon={CalendarClock} title="Calendar not available" body="This workspace's calendar isn't shared with you." />
      ) : agenda.isError ? (
        <EmptyState icon={CalendarClock} title="Couldn't load your schedule" body={getErrorMessage(agenda.error, "Please try again in a moment.")}>
          <button type="button" onClick={() => void agenda.refetch()} className={cn(smallButton, "border border-border text-foreground hover:bg-muted")}>
            Try again
          </button>
        </EmptyState>
      ) : items.length === 0 ? (
        <WeekEmpty
          title={(agenda.data?.items.length ?? 0) > 0 ? "Nothing else this week" : "Your week is clear"}
          canSchedule={canSchedule}
          canStart={canStart}
          onStart={onStart}
        />
      ) : (
        <ul className="divide-y divide-border">
          {items.map((item) => {
            const start = new Date(item.scheduledStartAt);
            const live = item.status === "LIVE";
            return (
              <li key={agendaItemKey(item)} className="flex items-center gap-3 py-3 first:pt-0 last:pb-0 sm:gap-4">
                <button type="button" onClick={() => setSelected(item)} className="flex min-w-0 flex-1 items-center gap-3 text-left sm:gap-4">
                  <span className={cn("flex w-[4.5rem] shrink-0 flex-col items-center rounded-xl px-1 py-2 sm:w-20", live ? "bg-primary/10 text-primary" : "bg-muted text-foreground")}>
                    <span className="text-[11px] font-medium uppercase">{start.toLocaleDateString(undefined, { weekday: "short" })}</span>
                    <span className="whitespace-nowrap text-xs font-bold tabular-nums">{start.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</span>
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate font-medium text-foreground">{item.meetingSpace.title || "CF Meet meeting"}</span>
                    <span className="block truncate text-xs text-muted-foreground">{item.meetingSpace.meetingCode}</span>
                  </span>
                </button>
                {live ? (
                  <button
                    type="button"
                    onClick={() => router.push(`/${encodeURIComponent(item.meetingSpace.meetingCode)}`)}
                    className={cn(smallButton, "bg-primary text-primary-foreground hover:bg-primary/90")}
                  >
                    <span className="size-1.5 animate-pulse rounded-full bg-white" /> Join
                  </button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {selected ? <OccurrenceDialog item={selected} onClose={() => setSelected(null)} /> : null}
    </Panel>
  );
}

function Spaces({ canSchedule }: { canSchedule: boolean }) {
  const { data, isLoading, error, refetch } = useSpaces();
  const featured = useUpcomingAgenda().data?.items[0]?.meetingSpaceId;
  const spaces = (data?.items ?? []).filter((s) => s.status === "ACTIVE" && s.id !== featured).slice(0, 6);
  return (
    <Panel
      title="Meeting spaces"
      action={
        <Link href="/dashboard/meetings" className="flex items-center gap-1 text-sm font-semibold text-primary hover:underline">
          All <ArrowRight className="size-3.5" />
        </Link>
      }
    >
      {isLoading ? (
        <SkeletonRows rows={2} />
      ) : error ? (
        <EmptyState icon={Video} title="Couldn't load spaces" body={getErrorMessage(error, "Please try again in a moment.")}>
          <button type="button" onClick={() => void refetch()} className={cn(smallButton, "border border-border text-foreground hover:bg-muted")}>
            Try again
          </button>
        </EmptyState>
      ) : spaces.length === 0 ? (
        <EmptyState icon={Video} art={<RoomArt className="size-16" />} title="No meeting spaces yet" body="A space is a reusable room with its own link, for a team or a regular meeting.">
          {canSchedule && (
            <Link href="/dashboard/meetings/new" className={cn(smallButton, "bg-primary text-primary-foreground hover:bg-primary/90")}>
              Create a space
            </Link>
          )}
        </EmptyState>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {spaces.map((s) => (
            <SpaceRow key={s.id} space={s} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SpaceRow({ space }: { space: MeetingSpaceResponse }) {
  const [copied, setCopied] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState("");
  const archive = useArchiveSpace();
  const perms = spacePermissions(space);
  // Archiving is owner-only and needs the version for the server's concurrency check.
  const canArchive = perms.owner && perms.active && space.version !== undefined;

  const copy = () => {
    void navigator.clipboard.writeText(`${window.location.origin}/${space.meetingCode}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const doArchive = async () => {
    setError("");
    try {
      await archive.mutateAsync({ id: space.id, version: space.version });
    } catch (e) {
      setError(getErrorMessage(e, "Could not archive this space."));
    }
    setConfirming(false);
  };

  return (
    <li className="flex items-center gap-3 rounded-xl border border-border px-3 py-2.5 transition-colors hover:bg-muted/50">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Video className="size-4" aria-hidden />
      </span>
      <Link href={`/dashboard/meetings/${space.id}`} className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium text-foreground">{space.title || "Untitled meeting"}</span>
        <span className="block truncate text-xs text-muted-foreground">{error || space.meetingCode}</span>
      </Link>
      <button
        type="button"
        onClick={copy}
        aria-label={copied ? "Link copied" : "Copy meeting link"}
        title="Copy link"
        className="flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      >
        {copied ? <Check className="size-4 text-[var(--success)]" /> : <Copy className="size-4" />}
      </button>
      {canArchive && (
        <button
          type="button"
          onClick={() => setConfirming(true)}
          aria-label={`Archive ${space.title || "space"}`}
          title="Archive"
          className="flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-destructive"
        >
          <Archive className="size-4" />
        </button>
      )}

      <Dialog open={confirming} onOpenChange={(v) => !archive.isPending && setConfirming(v)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Archive this meeting space?</DialogTitle>
            <DialogDescription>
              <span className="font-medium text-foreground">{space.title || "This space"}</span> ({space.meetingCode}) will be
              archived and its link will stop working. Past meetings and history are kept.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2 sm:gap-2">
            <button
              type="button"
              onClick={() => setConfirming(false)}
              disabled={archive.isPending}
              className="inline-flex h-10 items-center justify-center rounded-xl border border-border px-4 text-sm font-semibold text-foreground hover:bg-muted"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void doArchive()}
              disabled={archive.isPending}
              className="inline-flex h-10 items-center justify-center gap-2 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-70"
            >
              {archive.isPending && <Loader2 className="size-4 animate-spin" />}
              Archive space
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  );
}


/** The very next meeting: status, time, host, invite link and a one-click Join. */
function NextMeeting() {
  const router = useRouter();
  const agenda = useUpcomingAgenda();
  const { data: user } = useProfile();
  const [copied, setCopied] = useState(false);
  // "In 12 min" counts down while the page is open.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const next = agenda.data?.items[0];
  const start = next ? new Date(next.scheduledStartAt) : null;
  const minutes = start ? Math.round((start.getTime() - now) / 60_000) : 0;
  const live = next?.status === "LIVE";
  const status = live ? "Live now" : minutes <= 0 ? "Starting now" : minutes < 60 ? `In ${minutes} min` : "Upcoming";
  const time = start?.toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" });

  const copy = () => {
    if (!next) return;
    void navigator.clipboard.writeText(`${window.location.origin}/${next.meetingSpace.meetingCode}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <section className="flex flex-col rounded-3xl border border-primary/20 bg-primary/5 p-5 sm:p-8">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm font-medium text-muted-foreground">Next meeting</p>
        {next && (
          <span
            className={cn(
              "inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-semibold",
              live ? "bg-[var(--success)]/12 text-[var(--success)]" : "bg-primary/10 text-primary",
            )}
          >
            {live && <span className="size-1.5 animate-pulse rounded-full bg-[var(--success)]" />}
            {status}
          </span>
        )}
      </div>

      {agenda.isLoading ? (
        <div className="mt-4 space-y-3" aria-busy="true">
          <span className="block h-6 w-2/3 animate-pulse rounded bg-muted" />
          <span className="block h-4 w-1/2 animate-pulse rounded bg-muted" />
          <span className="block h-4 w-1/3 animate-pulse rounded bg-muted" />
        </div>
      ) : next ? (
        <>
          <p className="mt-3 line-clamp-2 break-words text-xl font-bold leading-snug text-foreground">{next.meetingSpace.title || "CF Meet meeting"}</p>
          <ul className="mt-4 space-y-2.5 text-sm text-muted-foreground">
            <li className="flex min-w-0 items-center gap-2.5">
              <CalendarClock className="size-4 shrink-0" aria-hidden />
              <span className="truncate">{time}</span>
            </li>
            <li className="flex items-center gap-2.5">
              <span className="flex size-4 items-center justify-center rounded-full bg-primary text-[9px] font-bold text-primary-foreground" aria-hidden>
                {(user?.username ?? "Y").slice(0, 1).toUpperCase()}
              </span>
              {next.relations.includes("OWNER") ? "Hosted by you" : next.relations.includes("INVITEE") ? "You're invited" : "You're a member"}
            </li>
            <li className="flex items-center gap-2.5">
              <Link2 className="size-4 shrink-0" aria-hidden />
              <span className="truncate font-mono text-foreground">{next.meetingSpace.meetingCode}</span>
            </li>
          </ul>
          <div className="mt-6 flex flex-wrap gap-2 lg:mt-auto lg:pt-6">
            <button
              type="button"
              onClick={() => router.push(`/${encodeURIComponent(next.meetingSpace.meetingCode)}`)}
              className="inline-flex h-11 items-center gap-2 rounded-xl bg-primary px-5 font-semibold text-primary-foreground transition-colors hover:bg-primary/90"
            >
              <Video className="size-4" /> {live ? "Join now" : "Open"}
            </button>
            <button
              type="button"
              onClick={copy}
              className="inline-flex h-11 items-center gap-2 rounded-xl border border-border px-4 text-sm font-semibold text-foreground transition-colors hover:bg-muted"
            >
              {copied ? <Check className="size-4 text-[var(--success)]" /> : <Copy className="size-4" />}
              {copied ? "Copied" : "Copy invite link"}
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="mt-3 text-xl font-bold leading-snug text-foreground">You&apos;re free</p>
          <p className="mt-2 text-sm text-muted-foreground">No meetings in the next 7 days. Anything you schedule shows up here.</p>
          <div className="mt-6 lg:mt-auto lg:pt-6">
            <Link
              href="/dashboard/meetings/new"
              className="inline-flex h-11 items-center gap-2 rounded-xl border border-border px-5 font-semibold text-foreground transition-colors hover:bg-muted"
            >
              <CalendarPlus className="size-4" /> Schedule a meeting
            </Link>
          </div>
        </>
      )}
    </section>
  );
}

/** Empty "Later this week": one quiet line and one action, nothing more. */
function WeekEmpty({ title, canSchedule, canStart, onStart }: { title: string; canSchedule: boolean; canStart: boolean; onStart: () => void }) {
  return (
    <div className="flex items-center gap-3 rounded-xl bg-muted/50 px-4 py-3">
      <CalendarArt className="size-9 shrink-0 sm:size-10" />
      <p className="min-w-0 flex-1 text-sm text-muted-foreground">
        <span className="font-medium text-foreground">{title}.</span>
        <span className="hidden sm:inline"> Nothing on your calendar for the next 7 days.</span>
      </p>
      {canSchedule ? (
        <Link href="/dashboard/meetings/new" className="shrink-0 text-sm font-semibold text-primary hover:underline">
          Schedule
        </Link>
      ) : canStart ? (
        <button type="button" onClick={onStart} className="shrink-0 text-sm font-semibold text-primary hover:underline">
          New meeting
        </button>
      ) : null}
    </div>
  );
}
