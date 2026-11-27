"use client";

import Link from "next/link";
import { ChevronLeft } from "lucide-react";
import { useConference } from "@/hooks/v2/useConferences";
import { useSpace } from "@/hooks/v2/useSpaces";
import { conferencePermission } from "@/lib/meeting-scheduling";
import { ConferenceRow } from "./ConferencesPanel";
import ParticipantsPanel from "./ParticipantsPanel";
import ConferenceRecordingsPanel from "./ConferenceRecordingsPanel";
import ConferenceArtifactsPanel from "./ConferenceArtifactsPanel";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";

/** One session of a meeting: its status and actions, the people in it, and its recordings. */
export default function ConferenceWorkspace({ id, spaceId }: { id: string; spaceId: string }) {
  const conference = useConference(id);
  const space = useSpace(spaceId);
  const belongsToSpace = conference.data?.meetingSpaceId === spaceId;
  const loading = conference.isLoading || space.isLoading;

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <Link href={`/dashboard/meetings/${spaceId}`} className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
        <ChevronLeft className="size-4" /> {space.data?.title || "Meeting"}
      </Link>
      <h1 className="mt-3 text-xl font-bold tracking-tight text-foreground sm:text-3xl">{conference.data?.scheduledStartAt ? new Date(conference.data.scheduledStartAt).toLocaleString(undefined, { weekday: "long", day: "numeric", month: "long", hour: "numeric", minute: "2-digit" }) : "Session"}</h1>
      <p className="mt-1 text-sm text-muted-foreground">One session of {space.data?.title || "this meeting"}.</p>

      {loading && (
        <div className="mt-6 space-y-4" aria-busy="true" aria-label="Loading session">
          <div className="h-32 animate-pulse rounded-2xl bg-muted" />
          <div className="h-64 animate-pulse rounded-2xl bg-muted" />
        </div>
      )}
      {conference.isError && <div className="mt-4"><RequestError error={conference.error} retry={() => void conference.refetch()} /></div>}
      {space.isError && <div className="mt-4"><RequestError error={space.error} retry={() => void space.refetch()} /></div>}
      {conference.data && !belongsToSpace && <p role="alert" className="mt-4 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">This session doesn&apos;t belong to this meeting.</p>}

      {conference.data && belongsToSpace && space.data && (
        <div className="mt-6 space-y-6">
          <ConferenceRow conference={conference.data} space={space.data} canManage={conferencePermission(space.data, "start")} onDetailsPage />
          {conference.data.view === "FULL" ? (
            <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)]">
              <div className="rounded-2xl border border-border bg-card p-5 sm:p-6">
                <ParticipantsPanel conferenceId={id} spaceRole={space.data.viewerRole} policy={space.data.rolePolicy} />
              </div>
              <div className="rounded-2xl border border-border bg-card p-5 sm:p-6">
                <ConferenceRecordingsPanel
                  conferenceId={id}
                  spaceRole={space.data.viewerRole}
                  rolePolicy={space.data.rolePolicy}
                  recordingPolicy={space.data.recordingPolicy}
                  live={conference.data.status === "LIVE"}
                />
              </div>
            </div>
          ) : null}
          {conference.data.view === "FULL" && (
            <div className="rounded-2xl border border-border bg-card p-5 sm:p-6">
              <ConferenceArtifactsPanel conferenceId={id} spaceRole={space.data.viewerRole} rolePolicy={space.data.rolePolicy} recordingPolicy={space.data.recordingPolicy} live={conference.data.status === "LIVE"} />
            </div>
          )}
          {conference.data.view !== "FULL" && (
            <p className="rounded-2xl border border-border bg-card p-6 text-sm text-muted-foreground">You can see this session&apos;s summary. Seeing people and managing the call needs host access.</p>
          )}
        </div>
      )}
    </div>
  );
}
