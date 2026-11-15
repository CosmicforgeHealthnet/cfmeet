"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { CalendarPlus, Check, ChevronLeft, ChevronRight, Copy, Globe, Loader2, Lock, Search, Shield, Users, Video } from "lucide-react";
import { useCreateInstantMeeting, useSpaces } from "@/hooks/v2/useSpaces";
import { useMeetingCapabilities } from "@/hooks/v2/useCalendar";
import type { AccessPolicy, MeetingSpaceResponse, SpaceStatus } from "@/types/v2";
import SpaceSchedule from "@/components/meetings/SpaceSchedule";
import { viewerTimeZone } from "@/lib/meeting-time";
import { getErrorMessage } from "@/utils/error";
import { cn } from "@/lib/utils";
import MeetingSpaceActions from "./MeetingSpaceActions";
import { RoomArt } from "./EmptyArt";

const FILTERS: { label: string; value: SpaceStatus | "all" }[] = [
  { label: "All", value: "all" },
  { label: "Active", value: "ACTIVE" },
  { label: "Archived", value: "ARCHIVED" },
];

const TYPE_LABELS: Record<string, string> = {
  STANDARD: "Meeting",
  WEBINAR: "Webinar",
  TOWN_HALL: "Town hall",
  HEALTHCARE: "Healthcare",
  TRAINING: "Training",
};

const ACCESS: Record<string, { label: string; icon: typeof Globe }> = {
  PUBLIC: { label: "Anyone with the link", icon: Globe },
  PUBLIC_PASSCODE: { label: "Passcode", icon: Lock },
  AUTHENTICATED: { label: "Signed-in users", icon: Users },
  ORGANIZATION_ONLY: { label: "Organization only", icon: Shield },
  DOMAIN_RESTRICTED: { label: "Domain only", icon: Shield },
  INVITE_ONLY: { label: "Invite only", icon: Lock },
  CUSTOM: { label: "Custom access", icon: Users },
  HEALTHCARE_APPOINTMENT: { label: "Appointment", icon: Shield },
};

const PER_PAGE = 10;

export default function MeetingsContent() {
  const router = useRouter();
  const [filter, setFilter] = useState<SpaceStatus | "all">("ACTIVE");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);
  const [instantError, setInstantError] = useState("");
  const { data, isLoading, isError, error, refetch } = useSpaces();
  const capabilities = useMeetingCapabilities();
  const canSchedule = capabilities.canCreatePersonal || capabilities.canCreateOrganization;
  const createInstant = useCreateInstantMeeting();

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (data?.items ?? []).filter(
      (s) =>
        (filter === "all" || s.status === filter) &&
        (!q || (s.title ?? "").toLowerCase().includes(q) || s.meetingCode.toLowerCase().includes(q)),
    );
  }, [data?.items, filter, query]);
  const counts = useMemo(() => {
    const items = data?.items ?? [];
    return {
      all: items.length,
      ACTIVE: items.filter((s) => s.status === "ACTIVE").length,
      ARCHIVED: items.filter((s) => s.status === "ARCHIVED").length,
    } as Record<SpaceStatus | "all", number>;
  }, [data?.items]);
  const pages = Math.max(1, Math.ceil(filtered.length / PER_PAGE));
  const current = Math.min(page, pages);
  const visible = filtered.slice((current - 1) * PER_PAGE, current * PER_PAGE);

  const startInstant = async () => {
    if (createInstant.isPending) return;
    setInstantError("");
    try {
      const { conference } = await createInstant.mutateAsync({ timezone: viewerTimeZone() });
      router.push(`/conferences/${encodeURIComponent(conference.id)}`);
    } catch (e) {
      setInstantError(getErrorMessage(e, "Could not start an instant meeting."));
    }
  };

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Header */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div>
          <h1 className="text-xl font-bold tracking-tight text-foreground sm:text-3xl">Meetings</h1>
          <p className="mt-1 text-sm text-muted-foreground">Your meeting rooms. Each one keeps its link and settings for next time.</p>
        </div>
        <div className="flex gap-2">
          {canSchedule && (
            <Link
              href="/dashboard/meetings/new"
              className="inline-flex h-11 items-center gap-2 rounded-xl border border-border bg-card px-4 text-sm font-semibold text-foreground transition-colors hover:bg-muted"
            >
              <CalendarPlus className="size-4" /> Schedule
            </Link>
          )}
          {capabilities.canCreatePersonal && (
            <button
              type="button"
              onClick={() => void startInstant()}
              disabled={createInstant.isPending}
              className="inline-flex h-11 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-80"
            >
              {createInstant.isPending ? <Loader2 className="size-4 animate-spin" /> : <Video className="size-4" />}
              {createInstant.isPending ? "Starting…" : "New meeting"}
            </button>
          )}
        </div>
      </div>
      {instantError && <p role="alert" className="mt-3 text-sm text-destructive">{instantError}</p>}

      {/* List card: tabs + search, then results */}
      <div className="mt-6 rounded-2xl border border-border bg-card">
        <div className="flex flex-col gap-3 border-b border-border px-5 pt-2 sm:flex-row sm:items-center sm:justify-between sm:pt-0">
          <div className="-mb-px flex gap-6" role="tablist" aria-label="Filter meetings">
            {FILTERS.map((f) => {
              const selected = filter === f.value;
              return (
                <button
                  key={f.value}
                  type="button"
                  role="tab"
                  aria-selected={selected}
                  onClick={() => {
                    setFilter(f.value);
                    setPage(1);
                  }}
                  className={cn(
                    "flex h-12 items-center gap-2 border-b-2 text-sm font-medium transition-colors",
                    selected ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground",
                  )}
                >
                  {f.label}
                  {!isLoading && (
                    <span
                      className={cn(
                        "min-w-5 rounded-full px-1.5 text-center text-xs tabular-nums",
                        selected ? "bg-primary/10 text-primary" : "bg-muted text-muted-foreground",
                      )}
                    >
                      {counts[f.value]}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
          <label className="relative block pb-3 sm:w-72 sm:py-2.5">
            <span className="sr-only">Search meetings</span>
            <Search className="pointer-events-none absolute left-3 top-[calc(50%-0.375rem)] size-4 -translate-y-1/2 text-muted-foreground sm:top-1/2" aria-hidden />
            <input
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setPage(1);
              }}
              placeholder="Search by name or code"
              className="h-10 w-full rounded-xl border-0 bg-muted/70 pl-9 pr-3 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-2 focus:ring-ring"
            />
          </label>
        </div>

        {isLoading ? (
          <ul aria-busy="true" aria-label="Loading meetings" className="divide-y divide-border">
            {[0, 1, 2].map((i) => (
              <li key={i} className="flex items-center gap-4 px-5 py-4">
                <span className="size-11 animate-pulse rounded-xl bg-muted" />
                <span className="flex-1 space-y-2">
                  <span className="block h-4 w-1/3 animate-pulse rounded bg-muted" />
                  <span className="block h-3 w-1/2 animate-pulse rounded bg-muted" />
                </span>
              </li>
            ))}
          </ul>
        ) : isError ? (
          <div className="flex flex-col items-center px-6 py-14 text-center">
            <p className="font-semibold text-foreground">Couldn&apos;t load your meetings</p>
            <p className="mt-1 text-sm text-muted-foreground">{getErrorMessage(error, "Please check your connection and try again.")}</p>
            <button
              type="button"
              onClick={() => void refetch()}
              className="mt-5 inline-flex h-10 items-center rounded-xl border border-border px-4 text-sm font-semibold text-foreground hover:bg-muted"
            >
              Try again
            </button>
          </div>
        ) : visible.length === 0 ? (
          <div className="flex flex-col items-center px-6 py-14 text-center">
            <RoomArt className="size-16" />
            <p className="mt-4 font-semibold text-foreground">
              {query ? "No matches" : filter === "ARCHIVED" ? "No archived meetings" : "No meetings yet"}
            </p>
            <p className="mt-1 max-w-sm text-sm text-muted-foreground">
              {query
                ? `Nothing matches "${query}". Try another name or code.`
                : filter === "ARCHIVED"
                  ? "Meetings you archive show up here."
                  : "Schedule a meeting or start one now. It gets its own link you can reuse."}
            </p>
            {!query && filter !== "ARCHIVED" && canSchedule && (
              <Link
                href="/dashboard/meetings/new"
                className="mt-5 inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90"
              >
                <CalendarPlus className="size-4" /> Schedule a meeting
              </Link>
            )}
          </div>
        ) : (
          <ul className="divide-y divide-border [&>li:last-child]:rounded-b-2xl">
            {visible.map((space) => (
              <MeetingRow key={space.id} space={space} />
            ))}
          </ul>
        )}
      </div>

      {/* Pagination */}
      {!isLoading && !isError && pages > 1 && (
        <div className="mt-4 flex items-center justify-between text-sm text-muted-foreground">
          <span>
            {(current - 1) * PER_PAGE + 1}–{Math.min(current * PER_PAGE, filtered.length)} of {filtered.length}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              aria-label="Previous page"
              disabled={current <= 1}
              onClick={() => setPage(current - 1)}
              className="flex size-9 items-center justify-center rounded-xl border border-border bg-card hover:bg-muted disabled:opacity-40"
            >
              <ChevronLeft className="size-4" />
            </button>
            <button
              type="button"
              aria-label="Next page"
              disabled={current >= pages}
              onClick={() => setPage(current + 1)}
              className="flex size-9 items-center justify-center rounded-xl border border-border bg-card hover:bg-muted disabled:opacity-40"
            >
              <ChevronRight className="size-4" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function AccessBadge({ policy }: { policy: AccessPolicy }) {
  const a = ACCESS[policy] ?? { label: policy, icon: Users };
  const Icon = a.icon;
  return (
    <span className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">
      <Icon className="size-3" aria-hidden /> {a.label}
    </span>
  );
}

function MeetingRow({ space }: { space: MeetingSpaceResponse }) {
  const [copied, setCopied] = useState(false);
  const archived = space.status === "ARCHIVED";
  const copy = () => {
    void navigator.clipboard.writeText(`${window.location.origin}/${space.meetingCode}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <li className="flex flex-col gap-3 px-5 py-4 transition-colors hover:bg-muted/40 sm:flex-row sm:items-center sm:gap-4">
      <div className="flex min-w-0 flex-1 items-start gap-4">
        <span
          className={cn(
            "flex size-11 shrink-0 items-center justify-center rounded-xl",
            archived ? "bg-muted text-muted-foreground" : "bg-primary/10 text-primary",
          )}
        >
          <Video className="size-5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <Link href={`/dashboard/meetings/${space.id}`} className="truncate font-semibold text-foreground hover:text-primary">
              {space.title || "Untitled meeting"}
            </Link>
            {archived && <span className="rounded-md bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">Archived</span>}
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs text-muted-foreground">
            <span className="rounded-md bg-primary/10 px-2 py-0.5 font-medium text-primary">{TYPE_LABELS[space.meetingType] ?? space.meetingType}</span>
            <AccessBadge policy={space.accessPolicy} />
            {space.organizationId && <span className="rounded-md bg-violet-500/10 px-2 py-0.5 font-medium text-violet-600 dark:text-violet-300">Organization</span>}
            <span className="inline-flex items-center gap-1">
              <SpaceSchedule space={space} compact />
            </span>
          </div>
        </div>
      </div>

      <div className="flex items-center gap-2 pl-15 sm:pl-0">
        <button
          type="button"
          onClick={copy}
          title="Copy meeting link"
          className="inline-flex h-9 items-center gap-1.5 rounded-xl border border-border px-3 font-mono text-xs text-foreground transition-colors hover:bg-muted"
        >
          {copied ? <Check className="size-3.5 text-[var(--success)]" /> : <Copy className="size-3.5 text-muted-foreground" />}
          {copied ? "Copied" : space.meetingCode}
        </button>
        <Link
          href={`/dashboard/meetings/${space.id}`}
          className="inline-flex h-9 items-center rounded-xl bg-primary/10 px-4 text-sm font-semibold text-primary transition-colors hover:bg-primary/15"
        >
          Open
        </Link>
        <MeetingSpaceActions space={space} />
      </div>
    </li>
  );
}
